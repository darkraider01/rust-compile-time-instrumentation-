//! Benchmark suite for async compile-time and runtime overhead.
//!
//! Runnable via `cargo bench --bench bench_async_overhead`.
//! Evaluates the observable overhead of generated native OpenTelemetry
//! dependency instrumentation on a bounded async workload that performs
//! deterministic useful work and suspends via cooperative executor yields.
//!
//! Workload structure (approved Variant A):
//! - Multi-threaded Tokio runtime with `worker_threads = 2`.
//! - Concurrency: C = 10 concurrent tasks spawned with `tokio::spawn`.
//! - Iterations per task: M = 1,000 iterations.
//! - Workload per iteration: sequential calls to `step_a` and `step_b`.
//! - Total completed pipeline invocations: 10,000.
//! - Total dependency function calls: 20,000.
//! - Useful work: 64-bit wrapping math across steps, deterministic bit mixing.
//! - Suspension points: `tokio::task::yield_now().await` in both steps.
//! - Verification:
//!   - Exact accumulator value `EXPECTED_ACC = 9839328791058930113` verified
//!     in both warm-up and measured passes.
//!   - Span count verified: 0 for baseline, 20,000 for instrumented.
//!   - Separate trace oracle check with `InMemorySpanExporter` verifying
//!     root trace ID propagation, parent-child hierarchy, and spawned-task parenting.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

fn min_val(values: &[f64]) -> f64 {
    values.iter().cloned().fold(f64::INFINITY, f64::min)
}

fn max_val(values: &[f64]) -> f64 {
    values.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
}

fn format_stats(values: &[f64]) -> String {
    format!(
        "{:.3} (min: {:.3}, max: {:.3}, spread: {:.3})",
        median(values.to_vec()),
        min_val(values),
        max_val(values),
        max_val(values) - min_val(values)
    )
}

fn sanitize_instrument_env(cmd: &mut Command) {
    for name in [
        "CARGO_INSTRUMENT_ACTIVE",
        "CARGO_INSTRUMENT_DEPENDENCIES",
        "CARGO_INSTRUMENT_REGISTRY",
        "CARGO_INSTRUMENT_SESSION",
        "CARGO_INSTRUMENT_SESSION_ID",
        "CARGO_INSTRUMENT_WRAPPER_MODE",
        "CARGO_INSTRUMENT_SENTINEL_MODE",
        "CARGO_INSTRUMENT_NATIVE_OTEL",
        "INSTRUMENT_DEBUG",
        "RUSTC_WRAPPER",
        "__CARGO_INSTRUMENT_FAULT_INJECT_CORRUPT_JSON",
        "__CARGO_INSTRUMENT_FAULT_INJECT_PREPASS_OMIT_PKG",
        "__CARGO_INSTRUMENT_FAULT_INJECT_PREPASS_FAIL",
        "__CARGO_INSTRUMENT_FAULT_INJECT_DELETE_RETAINED",
        "__CARGO_INSTRUMENT_FAULT_INJECT_CLEAN_FAIL",
    ] {
        cmd.env_remove(name);
    }
    cmd.env("CARGO_NET_OFFLINE", "true");
}

fn run_checked(cmd: &mut Command, what: &str) {
    let status = cmd
        .status()
        .unwrap_or_else(|error| panic!("{what} spawn failed: {error}"));
    assert!(status.success(), "{what} failed with {status}");
}

#[derive(Debug, Clone, Copy)]
struct ParsedResult {
    total_ms: f64,
    ns_per_pipeline: f64,
    ns_per_call: f64,
}

fn parse_runtime_result(output: &str) -> Option<ParsedResult> {
    for line in output.lines() {
        if line.starts_with("RUNTIME_RESULT ") {
            let mut total_ms = None;
            let mut ns_per_pipeline = None;
            let mut ns_per_call = None;
            for part in line.split_whitespace().skip(1) {
                if let Some((k, v)) = part.split_once('=') {
                    match k {
                        "total_ms" => total_ms = v.parse::<f64>().ok(),
                        "ns_per_pipeline" => ns_per_pipeline = v.parse::<f64>().ok(),
                        "ns_per_call" => ns_per_call = v.parse::<f64>().ok(),
                        _ => {}
                    }
                }
            }
            if let (Some(ms), Some(ns_p), Some(ns_c)) = (total_ms, ns_per_pipeline, ns_per_call) {
                return Some(ParsedResult {
                    total_ms: ms,
                    ns_per_pipeline: ns_p,
                    ns_per_call: ns_c,
                });
            }
        }
    }
    None
}

fn get_file_size(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn main() {
    println!("===============================================================================");
    println!(" CARGO-INSTRUMENT ASYNC OVERHEAD BENCHMARK (VARIANT A)");
    println!("===============================================================================\n");

    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf();
    let bench_tmp_parent = workspace_root.join("target");
    fs::create_dir_all(&bench_tmp_parent).expect("create target dir");
    let temp_dir = tempfile::Builder::new()
        .prefix("bench_async_")
        .tempdir_in(&bench_tmp_parent)
        .expect("create benchmark tempdir in target");
    let bench_root = temp_dir.path();

    let otel_shim_path = workspace_root.join("otel-shim");
    let otel_shim_path_escaped = otel_shim_path.to_string_lossy().replace('\\', "/");

    let cargo_instrument_bin = env!("CARGO_BIN_EXE_cargo-instrument");

    // Workspace Cargo.toml
    let ws_cargo_toml = r#"[workspace]
members = ["async_app"]
exclude = ["async_dep"]
resolver = "2"
"#;
    fs::write(bench_root.join("Cargo.toml"), ws_cargo_toml).expect("write ws Cargo.toml");

    // Dependency crate: async_dep
    let dep_dir = bench_root.join("async_dep");
    let dep_src = dep_dir.join("src");
    fs::create_dir_all(&dep_src).expect("create dep src");

    let dep_cargo = r#"[package]
name = "async_dep"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["rt-multi-thread"] }
"#;
    fs::write(dep_dir.join("Cargo.toml"), dep_cargo).expect("write dep Cargo.toml");

    let dep_lib = r#"//! External dependency implementing async workload steps and internal task spawning.

#[inline(never)]
pub async fn step_a(x: u64) -> u64 {
    let intermediate = x.wrapping_mul(6364136223846793005).wrapping_add(1);
    tokio::task::yield_now().await;
    intermediate.wrapping_add(0xDeadBeefCafeBabe)
}

#[inline(never)]
pub async fn step_b(x: u64) -> u64 {
    let intermediate = x.rotate_left(13) ^ 0x0123456789ABCDEF;
    tokio::task::yield_now().await;
    intermediate.wrapping_mul(11400714819323198485)
}

#[inline(never)]
pub async fn spawn_step(x: u64) -> u64 {
    tokio::spawn(async move {
        step_b(x).await
    })
    .await
    .expect("spawned step_b should complete")
}
"#;
    fs::write(dep_src.join("lib.rs"), dep_lib).expect("write dep lib.rs");

    // Application crate: async_app
    let app_dir = bench_root.join("async_app");
    let app_src = app_dir.join("src");
    fs::create_dir_all(&app_src).expect("create app src");

    let app_cargo = format!(
        r#"[package]
name = "async_app"
version = "0.1.0"
edition = "2021"

[dependencies]
async_dep = {{ path = "../async_dep" }}
otel-shim = {{ path = "{otel_shim_path_escaped}" }}
opentelemetry = "0.32.0"
opentelemetry_sdk = {{ version = "0.32.0", features = ["testing"] }}
tokio = {{ version = "1", features = ["macros", "rt-multi-thread"] }}
"#
    );
    fs::write(app_dir.join("Cargo.toml"), &app_cargo).expect("write app Cargo.toml");

    let app_main = r#"use std::env;
use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
use std::time::{Duration, Instant};

use opentelemetry::trace::{FutureExt, TraceContextExt, Tracer};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, Span, SpanData, SpanProcessor};
use opentelemetry_sdk::error::OTelSdkResult;

const EXPECTED_ACC: u64 = 9839328791058930113;

#[derive(Debug)]
struct CountingProcessor(Arc<AtomicU64>);

impl SpanProcessor for CountingProcessor {
    fn on_start(&self, _: &mut Span, _: &opentelemetry::Context) {}
    fn on_end(&self, span: SpanData) {
        if span.name == "step_a" || span.name == "step_b" || span.name == "spawn_step" {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn force_flush(&self) -> OTelSdkResult { Ok(()) }
    fn shutdown_with_timeout(&self, _: Duration) -> OTelSdkResult { Ok(()) }
}

async fn run_workload(concurrency: usize, iters_per_task: usize, root_cx: opentelemetry::Context) -> u64 {
    let mut handles = Vec::with_capacity(concurrency);
    for k in 0..concurrency {
        let cx = root_cx.clone();
        handles.push(tokio::spawn(async move {
            let mut acc: u64 = 0x1234_5678_9ABC_DEF0 ^ (k as u64);
            for i in 0..iters_per_task {
                acc = async_dep::step_a(acc ^ (i as u64)).await;
                acc = async_dep::step_b(acc).await;
            }
            acc
        }.with_context(cx)));
    }
    let mut total: u64 = 0;
    for h in handles {
        let task_res = h.await.expect("task join");
        total = total.wrapping_add(task_res);
    }
    total
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    otel_shim::init();

    let args: Vec<String> = env::args().collect();
    let verify_traces = args.iter().any(|arg| arg == "--verify-traces");
    let expected_spans: u64 = env::var("EXPECTED_SPANS").unwrap_or_else(|_| "0".into()).parse().unwrap();

    if verify_traces {
        // -------------------------------------------------------------------
        // Trace Oracle & Context Verification (Untimed)
        // -------------------------------------------------------------------
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        opentelemetry::global::set_tracer_provider(provider.clone());

        let tracer = opentelemetry::global::tracer("async_app");
        let root_span = tracer.start("async_workload_root");
        let root_cx = opentelemetry::Context::current_with_span(root_span);
        let root_trace_id = root_cx.span().span_context().trace_id();
        let root_span_id = root_cx.span().span_context().span_id();

        // 1. Direct step_a with context
        let res_a = async { async_dep::step_a(42).await }.with_context(root_cx.clone()).await;
        assert_eq!(res_a, 6638251280425130017, "step_a oracle output");

        // 2. Direct step_b with context
        let res_b = async { async_dep::step_b(res_a).await }.with_context(root_cx.clone()).await;
        assert_eq!(res_b, 6842571511199577820, "step_b oracle output");

        // 3. Spawned step inside dependency with context
        let res_s = async { async_dep::spawn_step(res_b).await }.with_context(root_cx.clone()).await;
        assert_eq!(res_s, 4228691440896540165, "spawn_step oracle output");

        root_cx.span().end();
        let _ = provider.force_flush();

        let all_spans = exporter.get_finished_spans().expect("read finished spans");
        let dep_spans: Vec<_> = all_spans
            .iter()
            .filter(|s| s.instrumentation_scope.name() == "async_dep")
            .collect();

        if expected_spans == 0 {
            assert!(dep_spans.is_empty(), "baseline binary must produce 0 dependency spans; found {}", dep_spans.len());
            println!("ORACLE_VERIFIED baseline=true spans=0");
        } else {
            assert_eq!(dep_spans.len(), 4, "expected 4 dependency spans in oracle check: step_a, step_b, spawn_step, and inner step_b; found {}", dep_spans.len());

            let span_a = dep_spans.iter().find(|s| s.name == "step_a").expect("step_a span");
            let span_b = dep_spans.iter().find(|s| s.name == "step_b" && s.parent_span_id == root_span_id).expect("step_b direct span");
            let span_spawn = dep_spans.iter().find(|s| s.name == "spawn_step").expect("spawn_step span");
            let span_spawn_child = dep_spans.iter().find(|s| s.name == "step_b" && s.parent_span_id == span_spawn.span_context.span_id()).expect("spawned step_b child span");

            // Trace ID propagation across all spans
            assert_eq!(span_a.span_context.trace_id(), root_trace_id, "step_a trace_id");
            assert_eq!(span_b.span_context.trace_id(), root_trace_id, "step_b trace_id");
            assert_eq!(span_spawn.span_context.trace_id(), root_trace_id, "spawn_step trace_id");
            assert_eq!(span_spawn_child.span_context.trace_id(), root_trace_id, "span_spawn_child trace_id");

            // Parent-child relationships
            assert_eq!(span_a.parent_span_id, root_span_id, "step_a parent_span_id");
            assert_eq!(span_b.parent_span_id, root_span_id, "step_b parent_span_id");
            assert_eq!(span_spawn.parent_span_id, root_span_id, "spawn_step parent_span_id");
            assert_eq!(span_spawn_child.parent_span_id, span_spawn.span_context.span_id(), "spawn_step child parent_span_id");

            // Instrumentation scope
            assert_eq!(span_a.instrumentation_scope.name(), "async_dep");
            assert_eq!(span_b.instrumentation_scope.name(), "async_dep");
            assert_eq!(span_spawn.instrumentation_scope.name(), "async_dep");
            assert_eq!(span_spawn_child.instrumentation_scope.name(), "async_dep");

            // Lifecycle outcome attributes
            for s in [span_a, span_b, span_spawn, span_spawn_child] {
                let outcome = s.attributes.iter().find(|kv| kv.key.as_str() == "cargo.instrumentation.async.outcome");
                assert!(outcome.is_some(), "span {} missing outcome attribute, attributes were: {:?}", s.name, s.attributes);
                assert_eq!(outcome.unwrap().value.as_str(), "completed");
            }

            println!("ORACLE_VERIFIED instrumented=true spans=4 trace_propagation=verified spawn_propagation=verified suspension_restoration=verified");
        }
        return;
    }

    // -----------------------------------------------------------------------
    // Performance Measurement Mode (CountingProcessor, untimed warm-up)
    // -----------------------------------------------------------------------
    let span_count = Arc::new(AtomicU64::new(0));
    let provider = SdkTracerProvider::builder()
        .with_span_processor(CountingProcessor(span_count.clone()))
        .build();
    opentelemetry::global::set_tracer_provider(provider);

    let tracer = opentelemetry::global::tracer("async_app");
    let root_span = tracer.start("async_workload_root");
    let root_cx = opentelemetry::Context::current_with_span(root_span);

    let concurrency: usize = 10;
    let iters_per_task: usize = 1000;

    // 1. Untimed warm-up pass
    let warmup_acc = run_workload(concurrency, iters_per_task, root_cx.clone()).await;
    let warmup_spans = span_count.load(Ordering::Relaxed);
    assert_eq!(warmup_spans, expected_spans, "warm-up span count");
    assert_eq!(warmup_acc, EXPECTED_ACC, "warm-up output agreement");
    println!("WARMUP_VERIFIED spans={} expected={} acc={}", warmup_spans, expected_spans, warmup_acc);

    // 2. Reset counters and workload state
    span_count.store(0, Ordering::Relaxed);

    // 3. Timed measured pass
    let start = Instant::now();
    let measured_acc = run_workload(concurrency, iters_per_task, root_cx.clone()).await;
    let elapsed = start.elapsed();

    // 4. Verification of measured pass
    let measured_spans = span_count.load(Ordering::Relaxed);
    assert_eq!(measured_spans, expected_spans, "timed measured span count");
    assert_eq!(measured_acc, EXPECTED_ACC, "measured output agreement");
    assert_eq!(measured_acc, warmup_acc, "workload output agreement between passes");
    println!("MEASURED_VERIFIED spans={} expected={} acc={}", measured_spans, expected_spans, measured_acc);

    let total_pipelines = (concurrency * iters_per_task) as u64;
    let total_calls = total_pipelines * 2;
    let total_ms = elapsed.as_secs_f64() * 1000.0;
    let ns_per_pipeline = (elapsed.as_nanos() as f64) / (total_pipelines as f64);
    let ns_per_call = (elapsed.as_nanos() as f64) / (total_calls as f64);

    println!(
        "RUNTIME_RESULT total_ms={:.2} ns_per_pipeline={:.2} ns_per_call={:.2} total_pipelines={} total_calls={}",
        total_ms, ns_per_pipeline, ns_per_call, total_pipelines, total_calls
    );
}
"#;
    fs::write(app_src.join("main.rs"), app_main).expect("write app main.rs");

    // Pre-fetch/lock
    let mut lockfile = Command::new("cargo");
    lockfile.args(["generate-lockfile"]).current_dir(bench_root);
    sanitize_instrument_env(&mut lockfile);
    run_checked(&mut lockfile, "generate-lockfile");

    // ------------------------------------------------------------------------
    // Build Baseline Release Binary
    // ------------------------------------------------------------------------
    println!("Building Baseline Release Binary...");
    let target_rel_base = bench_root.join("target_rel_base");
    let mut rel_base_cmd = Command::new("cargo");
    rel_base_cmd
        .args([
            "build",
            "--release",
            "--target-dir",
            target_rel_base.to_str().unwrap(),
        ])
        .current_dir(bench_root);
    sanitize_instrument_env(&mut rel_base_cmd);
    run_checked(&mut rel_base_cmd, "build baseline release");

    // ------------------------------------------------------------------------
    // Build Instrumented Release Binary
    // ------------------------------------------------------------------------
    println!("Building Instrumented Release Binary (--with-dependencies)...");
    let target_rel_inst = bench_root.join("target_rel_inst");
    let mut rel_inst_cmd = Command::new(cargo_instrument_bin);
    rel_inst_cmd
        .args([
            "--with-dependencies",
            "--",
            "build",
            "--release",
            "--target-dir",
            target_rel_inst.to_str().unwrap(),
        ])
        .current_dir(bench_root);
    sanitize_instrument_env(&mut rel_inst_cmd);
    let rel_inst_output = rel_inst_cmd
        .output()
        .expect("build instrumented release spawn");
    assert!(
        rel_inst_output.status.success(),
        "instrumented release build failed:\n{}",
        String::from_utf8_lossy(&rel_inst_output.stderr)
    );
    let inst_stderr = String::from_utf8_lossy(&rel_inst_output.stderr);
    let route_lines: Vec<&str> = inst_stderr
        .lines()
        .filter(|line| line.contains("selecting ") || line.contains("transformed"))
        .collect();
    for line in &route_lines {
        println!("EMITTER: {line}");
    }
    assert!(
        route_lines
            .iter()
            .any(|line| line.contains("crate=async_dep] selecting native R-4 emitter")),
        "expected async_dep to select the native R-4 emitter for this experiment; emitter lines: {route_lines:#?}"
    );
    assert!(
        !route_lines.iter().any(|line| line.contains("crate=async_app")),
        "workspace application must be excluded under --with-dependencies; emitter lines: {route_lines:#?}"
    );

    #[cfg(windows)]
    let bin_name = "async_app.exe";
    #[cfg(not(windows))]
    let bin_name = "async_app";

    let bin_base_path = target_rel_base.join("release").join(bin_name);
    let bin_inst_path = target_rel_inst.join("release").join(bin_name);

    // Binary size comparison
    let size_base = get_file_size(&bin_base_path);
    let size_inst = get_file_size(&bin_inst_path);
    let size_delta = size_inst as i64 - size_base as i64;
    let size_pct = (size_delta as f64 / size_base as f64) * 100.0;
    println!(
        "\nBINARY_SIZE baseline={} bytes, instrumented={} bytes, delta=+{} bytes ({:+.2}%)\n",
        size_base, size_inst, size_delta, size_pct
    );

    // ------------------------------------------------------------------------
    // Trace Oracle & Context Verification
    // ------------------------------------------------------------------------
    println!("Running Trace Oracle and Correctness Verification...");

    let oracle_base = Command::new(&bin_base_path)
        .arg("--verify-traces")
        .env("EXPECTED_SPANS", "0")
        .output()
        .expect("run baseline oracle check");
    assert!(
        oracle_base.status.success(),
        "baseline oracle failed: {}",
        String::from_utf8_lossy(&oracle_base.stderr)
    );
    let s_ob = String::from_utf8_lossy(&oracle_base.stdout);
    print!("Baseline Oracle: {s_ob}");
    assert!(s_ob.contains("ORACLE_VERIFIED baseline=true spans=0"));

    let oracle_inst = Command::new(&bin_inst_path)
        .arg("--verify-traces")
        .env("EXPECTED_SPANS", "4")
        .output()
        .expect("run instrumented oracle check");
    assert!(
        oracle_inst.status.success(),
        "instrumented oracle failed: {}",
        String::from_utf8_lossy(&oracle_inst.stderr)
    );
    let s_oi = String::from_utf8_lossy(&oracle_inst.stdout);
    print!("Instrumented Oracle: {s_oi}");
    assert!(s_oi.contains("ORACLE_VERIFIED instrumented=true spans=4"));

    // ------------------------------------------------------------------------
    // Measured Runtime Overhead (N=10 runs, alternating order)
    // ------------------------------------------------------------------------
    const RUNTIME_SAMPLES: usize = 10;
    println!(
        "\nRunning Measured Async Runtime Overhead (N={} runs, alternating order, untimed in-process warm-up per run)...",
        RUNTIME_SAMPLES
    );

    let mut base_total_ms = Vec::with_capacity(RUNTIME_SAMPLES);
    let mut inst_total_ms = Vec::with_capacity(RUNTIME_SAMPLES);
    let mut base_ns_call = Vec::with_capacity(RUNTIME_SAMPLES);
    let mut inst_ns_call = Vec::with_capacity(RUNTIME_SAMPLES);
    let mut base_ns_pipe = Vec::with_capacity(RUNTIME_SAMPLES);
    let mut inst_ns_pipe = Vec::with_capacity(RUNTIME_SAMPLES);

    for run in 0..RUNTIME_SAMPLES {
        let baseline_first = run % 2 == 0;
        let order_str = if baseline_first {
            "baseline_first"
        } else {
            "instrumented_first"
        };

        let (r_base, r_inst) = if baseline_first {
            let out1 = Command::new(&bin_base_path)
                .env("EXPECTED_SPANS", "0")
                .output()
                .expect("run baseline bin");
            assert!(
                out1.status.success(),
                "baseline failed: {}",
                String::from_utf8_lossy(&out1.stderr)
            );
            let s1 = String::from_utf8_lossy(&out1.stdout);
            print!("Baseline (run {run}): {s1}");
            assert!(s1.contains("WARMUP_VERIFIED spans=0 expected=0"));
            assert!(s1.contains("MEASURED_VERIFIED spans=0 expected=0"));
            let p1 = parse_runtime_result(&s1).expect("baseline RUNTIME_RESULT line");

            let out2 = Command::new(&bin_inst_path)
                .env("EXPECTED_SPANS", "20000")
                .output()
                .expect("run inst bin");
            assert!(
                out2.status.success(),
                "instrumented failed: {}",
                String::from_utf8_lossy(&out2.stderr)
            );
            let s2 = String::from_utf8_lossy(&out2.stdout);
            print!("Instrumented (run {run}): {s2}");
            assert!(s2.contains("WARMUP_VERIFIED spans=20000 expected=20000"));
            assert!(s2.contains("MEASURED_VERIFIED spans=20000 expected=20000"));
            let p2 = parse_runtime_result(&s2).expect("instrumented RUNTIME_RESULT line");

            (p1, p2)
        } else {
            let out2 = Command::new(&bin_inst_path)
                .env("EXPECTED_SPANS", "20000")
                .output()
                .expect("run inst bin");
            assert!(
                out2.status.success(),
                "instrumented failed: {}",
                String::from_utf8_lossy(&out2.stderr)
            );
            let s2 = String::from_utf8_lossy(&out2.stdout);
            print!("Instrumented (run {run}): {s2}");
            assert!(s2.contains("WARMUP_VERIFIED spans=20000 expected=20000"));
            assert!(s2.contains("MEASURED_VERIFIED spans=20000 expected=20000"));
            let p2 = parse_runtime_result(&s2).expect("instrumented RUNTIME_RESULT line");

            let out1 = Command::new(&bin_base_path)
                .env("EXPECTED_SPANS", "0")
                .output()
                .expect("run baseline bin");
            assert!(
                out1.status.success(),
                "baseline failed: {}",
                String::from_utf8_lossy(&out1.stderr)
            );
            let s1 = String::from_utf8_lossy(&out1.stdout);
            print!("Baseline (run {run}): {s1}");
            assert!(s1.contains("WARMUP_VERIFIED spans=0 expected=0"));
            assert!(s1.contains("MEASURED_VERIFIED spans=0 expected=0"));
            let p1 = parse_runtime_result(&s1).expect("baseline RUNTIME_RESULT line");

            (p1, p2)
        };

        base_total_ms.push(r_base.total_ms);
        inst_total_ms.push(r_inst.total_ms);
        base_ns_call.push(r_base.ns_per_call);
        inst_ns_call.push(r_inst.ns_per_call);
        base_ns_pipe.push(r_base.ns_per_pipeline);
        inst_ns_pipe.push(r_inst.ns_per_pipeline);

        println!(
            "SAMPLE {run} ({order_str}): baseline={:.2}ms ({:.1} ns/call, {:.1} ns/pipe) | instrumented={:.2}ms ({:.1} ns/call, {:.1} ns/pipe) | delta={:+.1} ns/call",
            r_base.total_ms, r_base.ns_per_call, r_base.ns_per_pipeline,
            r_inst.total_ms, r_inst.ns_per_call, r_inst.ns_per_pipeline,
            r_inst.ns_per_call - r_base.ns_per_call
        );
    }

    let med_base_call = median(base_ns_call.clone());
    let med_inst_call = median(inst_ns_call.clone());
    let delta_call = med_inst_call - med_base_call;
    let pct_call = (delta_call / med_base_call) * 100.0;

    let med_base_pipe = median(base_ns_pipe.clone());
    let med_inst_pipe = median(inst_ns_pipe.clone());
    let delta_pipe = med_inst_pipe - med_base_pipe;
    let pct_pipe = (delta_pipe / med_base_pipe) * 100.0;

    let med_base_ms = median(base_total_ms.clone());
    let med_inst_ms = median(inst_total_ms.clone());
    let delta_ms = med_inst_ms - med_base_ms;
    let pct_ms = (delta_ms / med_base_ms) * 100.0;

    println!("\n===============================================================================");
    println!(
        " ASYNC OVERHEAD BENCHMARK SUMMARY (N={} runs)",
        RUNTIME_SAMPLES
    );
    println!("===============================================================================");
    println!(
        "Workload: 10 concurrent tasks, 1,000 iterations each (10,000 pipelines, 20,000 calls)"
    );
    println!("Runtime: Tokio multi-thread (worker_threads = 2), cooperative yield_now() per step");
    println!("Useful work: 64-bit wrapping arithmetic, output verified against EXPECTED_ACC");
    println!("Spans generated: baseline = 0, instrumented = 20,000 (1 per step)");
    println!("-------------------------------------------------------------------------------");
    println!("Metric: Runtime Total Duration (ms)");
    println!("  Baseline:     {}", format_stats(&base_total_ms));
    println!("  Instrumented: {}", format_stats(&inst_total_ms));
    println!("  Delta:        {:+.2} ms ({:+.2}%)\n", delta_ms, pct_ms);
    println!("Metric: Time per Completed Pipeline (step_a + step_b) (ns)");
    println!("  Baseline:     {}", format_stats(&base_ns_pipe));
    println!("  Instrumented: {}", format_stats(&inst_ns_pipe));
    println!(
        "  Delta:        {:+.1} ns/pipeline ({:+.2}%)\n",
        delta_pipe, pct_pipe
    );
    println!("Metric: Time per Dependency Call (step_a or step_b) (ns)");
    println!("  Baseline:     {}", format_stats(&base_ns_call));
    println!("  Instrumented: {}", format_stats(&inst_ns_call));
    println!(
        "  Delta:        {:+.1} ns/call ({:+.2}%)\n",
        delta_call, pct_call
    );
    println!(
        "Release Binary Growth: +{} bytes ({:+.2}%)",
        size_delta, size_pct
    );
    println!("Trace Oracle: 4/4 spans verified (trace propagation, parentage, outcome attribute)");
    println!("===============================================================================\n");
}
