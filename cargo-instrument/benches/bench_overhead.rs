//! Benchmark suite for compile-time and runtime overhead (A17).
//!
//! Runnable on stable Rust via `cargo bench --bench bench_overhead`.
//! Measures compile-time (clean, repeat, incremental) across N=5 runs,
//! runtime overhead across N=10 runs (M=100,000 loop iterations, with an
//! untimed in-process workload warm-up pass before timing each sample) with
//! alternating baseline/instrumented order, and binary size deltas.
//!
//! Instrumented builds exercise the public dependency workflow explicitly:
//! `cargo-instrument --with-dependencies -- build` (policy `dependencies-v1`),
//! and the release build asserts that `bench_dep` selected the native R-4
//! emitter while the workspace application was left uninstrumented. Ambient
//! `CARGO_INSTRUMENT_*` settings (including the `CARGO_INSTRUMENT_ACTIVE`
//! recursion guard), `INSTRUMENT_DEBUG`, `RUSTC_WRAPPER`, and fault-injection
//! environment variables are removed from every child process so the ambient
//! environment cannot change the experiment.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

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
        "{:.3}s (min: {:.3}s, max: {:.3}s)",
        median(values.to_vec()),
        min_val(values),
        max_val(values)
    )
}

/// Child processes must not inherit ambient instrumentation settings: an
/// inherited `CARGO_INSTRUMENT_DEPENDENCIES`, `CARGO_INSTRUMENT_SESSION`, or
/// `RUSTC_WRAPPER` would silently change the workflow under measurement, and
/// an inherited recursion guard (`CARGO_INSTRUMENT_ACTIVE`) makes every
/// wrapper child treat itself as a nested invocation and skip instrumentation
/// entirely, producing uninstrumented "instrumented" samples. Nothing in the
/// CLI clears that guard before spawning Cargo, so the benchmark must.
/// Mirrors the `env_remove` pattern used by the e2e test CLI helper.
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
}

/// Every build subprocess must be checked: an unchecked failed setup build
/// would leave stale or missing artifacts and silently corrupt the samples.
fn run_checked(cmd: &mut Command, what: &str) {
    let status = cmd
        .status()
        .unwrap_or_else(|error| panic!("{what} spawn failed: {error}"));
    assert!(status.success(), "{what} failed with {status}");
}

fn main() {
    println!("===============================================================================");
    println!(" CARGO-INSTRUMENT OVERHEAD BENCHMARK SUITE (A17)");
    println!("===============================================================================\n");

    let temp_dir = tempfile::tempdir().expect("create benchmark tempdir");
    let bench_root = temp_dir.path();

    let otel_shim_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("otel-shim");
    let otel_shim_path_escaped = otel_shim_path.to_string_lossy().replace('\\', "/");

    let cargo_instrument_bin = env!("CARGO_BIN_EXE_cargo-instrument");

    // Create benchmark workspace with a multi-function library and application
    let ws_cargo_toml = r#"[workspace]
members = ["bench_app"]
exclude = ["bench_dep"]
resolver = "2"
"#;
    fs::write(bench_root.join("Cargo.toml"), ws_cargo_toml).expect("write ws Cargo.toml");

    // Dependency crate with 20 functions
    let dep_dir = bench_root.join("bench_dep");
    let dep_src = dep_dir.join("src");
    fs::create_dir_all(&dep_src).expect("create dep src");
    let dep_cargo = r#"[package]
name = "bench_dep"
version = "0.1.0"
edition = "2021"
"#;
    fs::write(dep_dir.join("Cargo.toml"), dep_cargo).expect("write dep Cargo.toml");

    let mut dep_lib = String::from("// 20 functions in dependency\n");
    for i in 0..20 {
        dep_lib.push_str(&format!(
            "#[inline(never)]\npub fn compute_step_{i}(x: u64) -> u64 {{ (x.wrapping_mul(6364136223846793005)).wrapping_add({i}) }}\n"
        ));
    }
    fs::write(dep_src.join("lib.rs"), &dep_lib).expect("write dep lib.rs");

    // Application crate
    let app_dir = bench_root.join("bench_app");
    let app_src = app_dir.join("src");
    fs::create_dir_all(&app_src).expect("create app src");

    let app_cargo = format!(
        r#"[package]
name = "bench_app"
version = "0.1.0"
edition = "2021"

[dependencies]
bench_dep = {{ path = "../bench_dep" }}
otel-shim = {{ path = "{otel_shim_path_escaped}" }}
opentelemetry = "0.32.0"
opentelemetry_sdk = "0.32.0"
"#
    );
    fs::write(app_dir.join("Cargo.toml"), &app_cargo).expect("write app Cargo.toml");

    let app_main = r#"use std::time::{Duration, Instant};
use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
use opentelemetry_sdk::trace::{Span, SpanData, SpanProcessor};
use opentelemetry_sdk::error::OTelSdkResult;

#[derive(Debug)]
struct CountingProcessor(Arc<AtomicU64>);

impl SpanProcessor for CountingProcessor {
    fn on_start(&self, _: &mut Span, _: &opentelemetry::Context) {}
    fn on_end(&self, span: SpanData) {
        if span.name.starts_with("compute_step_") {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn force_flush(&self) -> OTelSdkResult { Ok(()) }
    fn shutdown_with_timeout(&self, _: Duration) -> OTelSdkResult { Ok(()) }
}

#[inline(never)]
fn run_workload(iterations: u64) -> u64 {
    let mut acc: u64 = 1;
    for i in 0..iterations {
        acc = bench_dep::compute_step_0(acc ^ i);
        acc = bench_dep::compute_step_1(acc);
        acc = bench_dep::compute_step_2(acc);
        acc = bench_dep::compute_step_3(acc);
        acc = bench_dep::compute_step_4(acc);
    }
    acc
}

const EXPECTED_ACC: u64 = 2033737868462570593;

fn main() {
    otel_shim::init();
    let span_count = Arc::new(AtomicU64::new(0));
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_span_processor(CountingProcessor(span_count.clone())).build();
    opentelemetry::global::set_tracer_provider(provider);

    let iterations: u64 = 100_000;
    let expected: u64 = std::env::var("EXPECTED_SPANS").unwrap().parse().unwrap();

    // 1. Untimed in-process warm-up pass
    let warmup_acc = run_workload(iterations);
    let warmup_spans = span_count.load(Ordering::Relaxed);
    assert_eq!(warmup_spans, expected, "warm-up dependency span count");
    assert_eq!(warmup_acc, EXPECTED_ACC, "warm-up workload output agreement");
    println!("WARMUP_VERIFIED spans={} expected={} acc={}", warmup_spans, expected, warmup_acc);

    // 2. Reset counters and workload state prior to timing
    span_count.store(0, Ordering::Relaxed);

    // 3. Timed measured pass
    let start = Instant::now();
    let measured_acc = run_workload(iterations);
    let elapsed = start.elapsed();

    // 4. Verification of measured pass
    let measured_spans = span_count.load(Ordering::Relaxed);
    assert_eq!(measured_spans, expected, "timed dependency span count");
    assert_eq!(measured_acc, EXPECTED_ACC, "measured workload output agreement");
    assert_eq!(measured_acc, warmup_acc, "workload output agreement between passes");
    println!("MEASURED_VERIFIED spans={} expected={} acc={}", measured_spans, expected, measured_acc);

    let total_calls = iterations * 5;
    let ns_per_call = (elapsed.as_nanos() as f64) / (total_calls as f64);
    println!("RUNTIME_RESULT total_ms={:.2} ns_per_call={:.2} total_calls={}", elapsed.as_secs_f64() * 1000.0, ns_per_call, total_calls);
}
"#;
    fs::write(app_src.join("main.rs"), app_main).expect("write app main.rs");

    // Pre-fetch/lock
    let mut lockfile = Command::new("cargo");
    lockfile.args(["generate-lockfile"]).current_dir(bench_root);
    sanitize_instrument_env(&mut lockfile);
    run_checked(&mut lockfile, "generate-lockfile");

    const BUILD_SAMPLES: usize = 5;
    const RUNTIME_SAMPLES: usize = 10;

    // ------------------------------------------------------------------------
    // 1. COMPILE TIME: Clean Builds
    // ------------------------------------------------------------------------
    println!(
        "Running Compile-Time Benchmarks (N={} runs each, alternating order)...",
        BUILD_SAMPLES
    );

    let mut baseline_clean = Vec::with_capacity(BUILD_SAMPLES);
    let mut instrumented_clean = Vec::with_capacity(BUILD_SAMPLES);

    for run in 0..BUILD_SAMPLES {
        let baseline_first = run % 2 == 0;
        let order_str = if baseline_first {
            "baseline_first"
        } else {
            "instrumented_first"
        };

        let target_base = bench_root.join(format!("target_base_{run}"));
        let mut base_cmd = Command::new("cargo");
        base_cmd
            .args(["build", "--target-dir", target_base.to_str().unwrap()])
            .current_dir(bench_root);
        sanitize_instrument_env(&mut base_cmd);

        let target_inst = bench_root.join(format!("target_inst_{run}"));
        let mut inst_cmd = Command::new(cargo_instrument_bin);
        inst_cmd
            .args([
                "--with-dependencies",
                "--",
                "build",
                "--target-dir",
                target_inst.to_str().unwrap(),
            ])
            .current_dir(bench_root);
        sanitize_instrument_env(&mut inst_cmd);

        let (base_time, inst_time) = if baseline_first {
            let t0 = Instant::now();
            run_checked(&mut base_cmd, "baseline clean build");
            let d_base = t0.elapsed().as_secs_f64();

            let t1 = Instant::now();
            run_checked(&mut inst_cmd, "instrumented clean build");
            let d_inst = t1.elapsed().as_secs_f64();

            (d_base, d_inst)
        } else {
            let t1 = Instant::now();
            run_checked(&mut inst_cmd, "instrumented clean build");
            let d_inst = t1.elapsed().as_secs_f64();

            let t0 = Instant::now();
            run_checked(&mut base_cmd, "baseline clean build");
            let d_base = t0.elapsed().as_secs_f64();

            (d_base, d_inst)
        };

        baseline_clean.push(base_time);
        instrumented_clean.push(inst_time);

        println!(
            "RAW sample {run}: baseline_clean={:.3}s instrumented_clean={:.3}s order={order_str}",
            base_time, inst_time
        );
    }

    // ------------------------------------------------------------------------
    // 2. COMPILE TIME: Repeat Builds (no-op)
    // ------------------------------------------------------------------------
    let target_base_repeat = bench_root.join("target_base_repeat");
    let mut setup_base = Command::new("cargo");
    setup_base
        .args([
            "build",
            "--target-dir",
            target_base_repeat.to_str().unwrap(),
        ])
        .current_dir(bench_root);
    sanitize_instrument_env(&mut setup_base);
    run_checked(&mut setup_base, "setup baseline repeat");

    let target_inst_repeat = bench_root.join("target_inst_repeat");
    let mut setup_inst = Command::new(cargo_instrument_bin);
    setup_inst.args([
        "--with-dependencies",
        "--",
        "build",
        "--target-dir",
        target_inst_repeat.to_str().unwrap(),
    ]);
    setup_inst.current_dir(bench_root);
    sanitize_instrument_env(&mut setup_inst);
    run_checked(&mut setup_inst, "setup instrumented repeat");

    let mut baseline_repeat = Vec::with_capacity(BUILD_SAMPLES);
    let mut instrumented_repeat = Vec::with_capacity(BUILD_SAMPLES);

    for run in 0..BUILD_SAMPLES {
        let baseline_first = run % 2 == 0;
        let order_str = if baseline_first {
            "baseline_first"
        } else {
            "instrumented_first"
        };

        let mut base_cmd = Command::new("cargo");
        base_cmd
            .args([
                "build",
                "--target-dir",
                target_base_repeat.to_str().unwrap(),
            ])
            .current_dir(bench_root);
        sanitize_instrument_env(&mut base_cmd);

        let mut inst_cmd = Command::new(cargo_instrument_bin);
        inst_cmd
            .args([
                "--with-dependencies",
                "--",
                "build",
                "--target-dir",
                target_inst_repeat.to_str().unwrap(),
            ])
            .current_dir(bench_root);
        sanitize_instrument_env(&mut inst_cmd);

        let (base_time, inst_time) = if baseline_first {
            let t0 = Instant::now();
            run_checked(&mut base_cmd, "baseline repeat build");
            let d_base = t0.elapsed().as_secs_f64();

            let t1 = Instant::now();
            run_checked(&mut inst_cmd, "instrumented repeat build");
            let d_inst = t1.elapsed().as_secs_f64();

            (d_base, d_inst)
        } else {
            let t1 = Instant::now();
            run_checked(&mut inst_cmd, "instrumented repeat build");
            let d_inst = t1.elapsed().as_secs_f64();

            let t0 = Instant::now();
            run_checked(&mut base_cmd, "baseline repeat build");
            let d_base = t0.elapsed().as_secs_f64();

            (d_base, d_inst)
        };

        baseline_repeat.push(base_time);
        instrumented_repeat.push(inst_time);

        println!(
            "RAW sample {run}: baseline_repeat={:.3}s instrumented_repeat={:.3}s order={order_str}",
            base_time, inst_time
        );
    }

    // ------------------------------------------------------------------------
    // 3. COMPILE TIME: Incremental App Builds (app modified)
    // ------------------------------------------------------------------------
    let mut baseline_inc = Vec::with_capacity(BUILD_SAMPLES);
    let mut instrumented_inc = Vec::with_capacity(BUILD_SAMPLES);

    for run in 0..BUILD_SAMPLES {
        let baseline_first = run % 2 == 0;
        let order_str = if baseline_first {
            "baseline_first"
        } else {
            "instrumented_first"
        };

        // Touch main.rs
        std::thread::sleep(Duration::from_millis(50));
        let touched_main = format!("{}\n// modification {run}\n", app_main);
        fs::write(app_src.join("main.rs"), touched_main).unwrap();

        let mut base_cmd = Command::new("cargo");
        base_cmd
            .args([
                "build",
                "--target-dir",
                target_base_repeat.to_str().unwrap(),
            ])
            .current_dir(bench_root);
        sanitize_instrument_env(&mut base_cmd);

        let mut inst_cmd = Command::new(cargo_instrument_bin);
        inst_cmd
            .args([
                "--with-dependencies",
                "--",
                "build",
                "--target-dir",
                target_inst_repeat.to_str().unwrap(),
            ])
            .current_dir(bench_root);
        sanitize_instrument_env(&mut inst_cmd);

        let (base_time, inst_time) = if baseline_first {
            let t0 = Instant::now();
            run_checked(&mut base_cmd, "baseline incremental build");
            let d_base = t0.elapsed().as_secs_f64();

            let t1 = Instant::now();
            run_checked(&mut inst_cmd, "instrumented incremental build");
            let d_inst = t1.elapsed().as_secs_f64();

            (d_base, d_inst)
        } else {
            let t1 = Instant::now();
            run_checked(&mut inst_cmd, "instrumented incremental build");
            let d_inst = t1.elapsed().as_secs_f64();

            let t0 = Instant::now();
            run_checked(&mut base_cmd, "baseline incremental build");
            let d_base = t0.elapsed().as_secs_f64();

            (d_base, d_inst)
        };

        baseline_inc.push(base_time);
        instrumented_inc.push(inst_time);

        println!(
            "RAW sample {run}: baseline_inc={:.3}s instrumented_inc={:.3}s order={order_str}",
            base_time, inst_time
        );
    }

    // ------------------------------------------------------------------------
    // 4. RUNTIME OVERHEAD: 100k invocations (500k function calls)
    // ------------------------------------------------------------------------
    println!("\nRunning Runtime Overhead Benchmarks (M=100,000 iterations)...");

    // Build release binary baseline
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

    // Build release binary instrumented; capture diagnostics so the emitter
    // route of this experiment is recorded (and asserted) rather than assumed.
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
            .any(|line| line.contains("crate=bench_dep] selecting native R-4 emitter")),
        "expected bench_dep to select the native R-4 emitter for this experiment; emitter lines: {route_lines:#?}"
    );
    assert!(
        !route_lines.iter().any(|line| line.contains("crate=bench_app")),
        "workspace application must be excluded under --with-dependencies; emitter lines: {route_lines:#?}"
    );

    #[cfg(windows)]
    let bin_name = "bench_app.exe";
    #[cfg(not(windows))]
    let bin_name = "bench_app";

    let bin_base_path = target_rel_base.join("release").join(bin_name);
    let bin_inst_path = target_rel_inst.join("release").join(bin_name);

    println!(
        "\nRunning Measured Runtime Overhead (N={} runs, alternating order, untimed in-process warm-up per run)...",
        RUNTIME_SAMPLES
    );
    let mut runtime_base_ns = Vec::with_capacity(RUNTIME_SAMPLES);
    let mut runtime_inst_ns = Vec::with_capacity(RUNTIME_SAMPLES);

    for run in 0..RUNTIME_SAMPLES {
        let baseline_first = run % 2 == 0;
        let order_str = if baseline_first {
            "baseline_first"
        } else {
            "instrumented_first"
        };

        let (base_ns, inst_ns) = if baseline_first {
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
            assert!(
                s1.contains("WARMUP_VERIFIED spans=0 expected=0"),
                "baseline missing warm-up verification: {s1}"
            );
            assert!(
                s1.contains("MEASURED_VERIFIED spans=0 expected=0"),
                "baseline missing measured verification: {s1}"
            );
            let ns1 = parse_ns_per_call(&s1).expect("baseline RUNTIME_RESULT line");

            let out2 = Command::new(&bin_inst_path)
                .env("EXPECTED_SPANS", "500000")
                .output()
                .expect("run inst bin");
            assert!(
                out2.status.success(),
                "instrumented failed: {}",
                String::from_utf8_lossy(&out2.stderr)
            );
            let s2 = String::from_utf8_lossy(&out2.stdout);
            print!("Instrumented (run {run}): {s2}");
            assert!(
                s2.contains("WARMUP_VERIFIED spans=500000 expected=500000"),
                "instrumented missing warm-up verification: {s2}"
            );
            assert!(
                s2.contains("MEASURED_VERIFIED spans=500000 expected=500000"),
                "instrumented missing measured verification: {s2}"
            );
            let ns2 = parse_ns_per_call(&s2).expect("instrumented RUNTIME_RESULT line");

            (ns1, ns2)
        } else {
            let out2 = Command::new(&bin_inst_path)
                .env("EXPECTED_SPANS", "500000")
                .output()
                .expect("run inst bin");
            assert!(
                out2.status.success(),
                "instrumented failed: {}",
                String::from_utf8_lossy(&out2.stderr)
            );
            let s2 = String::from_utf8_lossy(&out2.stdout);
            print!("Instrumented (run {run}): {s2}");
            assert!(
                s2.contains("WARMUP_VERIFIED spans=500000 expected=500000"),
                "instrumented missing warm-up verification: {s2}"
            );
            assert!(
                s2.contains("MEASURED_VERIFIED spans=500000 expected=500000"),
                "instrumented missing measured verification: {s2}"
            );
            let ns2 = parse_ns_per_call(&s2).expect("instrumented RUNTIME_RESULT line");

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
            assert!(
                s1.contains("WARMUP_VERIFIED spans=0 expected=0"),
                "baseline missing warm-up verification: {s1}"
            );
            assert!(
                s1.contains("MEASURED_VERIFIED spans=0 expected=0"),
                "baseline missing measured verification: {s1}"
            );
            let ns1 = parse_ns_per_call(&s1).expect("baseline RUNTIME_RESULT line");

            (ns1, ns2)
        };

        runtime_base_ns.push(base_ns);
        runtime_inst_ns.push(inst_ns);

        println!(
            "RAW sample {run}: baseline_runtime={:.2}ns instrumented_runtime={:.2}ns order={order_str}",
            base_ns, inst_ns
        );
    }

    // ------------------------------------------------------------------------
    // 5. BINARY SIZE DELTA
    // ------------------------------------------------------------------------
    let size_base = fs::metadata(&bin_base_path).map(|m| m.len()).unwrap_or(0);
    let size_inst = fs::metadata(&bin_inst_path).map(|m| m.len()).unwrap_or(0);
    let size_delta_pct = if size_base > 0 {
        ((size_inst as f64 - size_base as f64) / size_base as f64) * 100.0
    } else {
        0.0
    };

    // ------------------------------------------------------------------------
    // REPORT TABLES
    // ------------------------------------------------------------------------
    let clean_base_med = median(baseline_clean.clone());
    let clean_inst_med = median(instrumented_clean.clone());
    let clean_delta = ((clean_inst_med - clean_base_med) / clean_base_med) * 100.0;

    let rep_base_med = median(baseline_repeat.clone());
    let rep_inst_med = median(instrumented_repeat.clone());
    let rep_delta = ((rep_inst_med - rep_base_med) / rep_base_med) * 100.0;

    let inc_base_med = median(baseline_inc.clone());
    let inc_inst_med = median(instrumented_inc.clone());
    let inc_delta = ((inc_inst_med - inc_base_med) / inc_base_med) * 100.0;

    let rt_base_med = median(runtime_base_ns.clone());
    let rt_inst_med = median(runtime_inst_ns.clone());
    let rt_overhead_ns = rt_inst_med - rt_base_med;

    println!("\n### A17 Benchmark Results Table\n");
    println!(
        "#### Compile-Time Overhead (N={} runs each, medians reported)\n",
        BUILD_SAMPLES
    );
    println!("| Build Type | Baseline (Uninstrumented) | Instrumented | Overhead Delta |");
    println!("|---|---|---|---|");
    println!(
        "| Clean Build | {} | {} | {:+.1}% |",
        format_stats(&baseline_clean),
        format_stats(&instrumented_clean),
        clean_delta
    );
    println!(
        "| Repeat Build (no-op) | {} | {} | {:+.1}% |",
        format_stats(&baseline_repeat),
        format_stats(&instrumented_repeat),
        rep_delta
    );
    println!(
        "| Incremental Build (app) | {} | {} | {:+.1}% |",
        format_stats(&baseline_inc),
        format_stats(&instrumented_inc),
        inc_delta
    );
    println!(
        "\n* Instrumented builds use the explicit public dependency workflow `cargo instrument --with-dependencies -- build` (policy `dependencies-v1`) with ambient `CARGO_INSTRUMENT_*`, `INSTRUMENT_DEBUG`, `RUSTC_WRAPPER`, and fault-injection variables removed; instrumented compile times are public-command wall time including the JSON pre-pass, orchestration, mirroring, and rewriting. The release build asserts and prints the emitter route (`EMITTER:` lines above): native R-4 for `bench_dep`, no wrapper instrumentation for `bench_app`. Runtime measures generated dependency spans with an OpenTelemetry SDK counting processor and no exporter; provider setup and `otel_shim::init()` run before the timed loop. Each measured sample executes an untimed in-process workload pass (verifying warm-up span counts and exact workload output), resets the counter and accumulator, and then measures the timed pass. Each instrumented sample asserts 500,000 completed dependency spans inside the application; each baseline asserts zero. Build and runtime pairs alternate baseline-first and instrumented-first order. Measured runtime samples (N={}) use only the timed pass for medians. Raw per-run samples are printed as `RAW sample` lines; only medians feed the tables. This synchronous microbenchmark does not measure async application overhead, and parent-child ancestry of the counted spans is not verified here.",
        RUNTIME_SAMPLES
    );

    println!(
        "\n#### Runtime Overhead (N={} runs, untimed in-process warm-up per run, M=100,000 loop iterations, 500,000 calls)\n",
        RUNTIME_SAMPLES
    );
    println!("| Metric | Baseline | Instrumented (SDK counting processor) | Delta / Overhead |");
    println!("|---|---|---|---|");
    println!(
        "| Latency per call | {:.2} ns | {:.2} ns | {:+.2} ns/call |",
        rt_base_med, rt_inst_med, rt_overhead_ns
    );
    println!(
        "| Throughput | {:.2}M calls/sec | {:.2}M calls/sec | - |",
        1000.0 / rt_base_med,
        1000.0 / rt_inst_med
    );

    println!("\n#### Binary Size Delta (Release profile)\n");
    println!("| Binary | Size (bytes) | Delta |");
    println!("|---|---|---|");
    println!("| Baseline | {} bytes | - |", size_base);
    println!(
        "| Instrumented | {} bytes | {:+.1}% ({:+} bytes) |",
        size_inst,
        size_delta_pct,
        (size_inst as i64 - size_base as i64)
    );

    println!("\nBenchmark suite completed successfully.");
}

fn parse_ns_per_call(stdout: &str) -> Option<f64> {
    for line in stdout.lines() {
        if line.starts_with("RUNTIME_RESULT") {
            for part in line.split_whitespace() {
                if let Some(val) = part.strip_prefix("ns_per_call=") {
                    return val.parse().ok();
                }
            }
        }
    }
    None
}
