//! Isolated P2.4/R-4 architecture probe.
//!
//! This is intentionally an opt-in test-only path, not a production dependency-injection
//! interface. The wrapper receives an exact Cargo-produced rlib path; it never discovers an
//! OpenTelemetry artifact by globbing a dependency directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cargo_instrument::SessionPlan;

fn run_cargo(workspace: &Path, target_dir: &Path, args: &[&str], session_file: &Path) -> Output {
    Command::new("cargo")
        .args(args)
        .arg("--offline")
        .arg("--target-dir")
        .arg(target_dir)
        .current_dir(workspace)
        .env("RUSTC_WRAPPER", env!("CARGO_BIN_EXE_cargo-instrument"))
        .env("CARGO_INSTRUMENT_SESSION", session_file)
        .env("CARGO_INSTRUMENT_WRAPPER_MODE", "1")
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("run cargo through R-4 wrapper")
}

fn assert_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn find_mirrored_dependency_source(root: &Path) -> Option<PathBuf> {
    for entry in fs::read_dir(root).ok()?.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_mirrored_dependency_source(&path) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|name| name == "lib.rs")
            && fs::read_to_string(&path)
                .is_ok_and(|contents| contents.contains("__cargo_instrument_anchor"))
        {
            return Some(path);
        }
    }
    None
}

fn find_mirrored_sources(root: &Path, marker: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(root)
        .expect("read mirror root")
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path.is_dir() {
            found.extend(find_mirrored_sources(&path, marker));
        } else if path.file_name().is_some_and(|name| name == "lib.rs")
            && fs::read_to_string(&path).is_ok_and(|contents| contents.contains(marker))
        {
            found.push(path);
        }
    }
    found
}

fn snapshot_tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut snapshot = Vec::new();
    for entry in fs::read_dir(root).expect("read source tree") {
        let entry = entry.expect("read source entry");
        let path = entry.path();
        if path.is_dir() {
            snapshot.extend(snapshot_tree(&path));
        } else if path.is_file() {
            snapshot.push((path, fs::read(entry.path()).expect("read source file")));
        }
    }
    snapshot.sort_by(|left, right| left.0.cmp(&right.0));
    snapshot
}

fn cached_otel_source() -> PathBuf {
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".cargo")))
        .expect("find Cargo home");
    let registry_src = cargo_home.join("registry").join("src");
    for registry in fs::read_dir(registry_src).expect("read Cargo registry source") {
        let candidate = registry
            .expect("read Cargo registry entry")
            .path()
            .join("opentelemetry-0.32.0");
        if candidate.is_dir() {
            return candidate;
        }
    }
    panic!("find cached opentelemetry-0.32.0 source");
}

fn prepare_r4_session(workspace: &Path, target: &Path) -> (SessionPlan, PathBuf, PathBuf) {
    let metadata_output = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--offline"])
        .current_dir(workspace)
        .output()
        .expect("read Cargo metadata for R-4 plan");
    assert_success(&metadata_output, "R-4 Cargo metadata");
    let metadata: serde_json::Value =
        serde_json::from_slice(&metadata_output.stdout).expect("parse Cargo metadata JSON");

    let prepass = Command::new("cargo")
        .args([
            "build",
            "--package",
            "otel_provider",
            "--offline",
            "--message-format=json",
            "--target-dir",
        ])
        .arg(target)
        .current_dir(workspace)
        .output()
        .expect("run R-4 artifact pre-pass");
    assert_success(&prepass, "R-4 artifact pre-pass");

    let mut plan = SessionPlan::from_metadata_json(&metadata).expect("build R-4 session plan");
    plan.add_r4_artifacts_from_cargo_json(&metadata, &prepass.stdout, None)
        .expect("capture authoritative R-4 Cargo artifact");
    assert_eq!(
        plan.r4_native_otel_artifacts.len(),
        1,
        "fixture should produce one deterministic native OpenTelemetry artifact"
    );
    let rlib = plan.r4_native_otel_artifacts[0].rlib_path.clone();
    let session_file = target.join("r4-session.json");
    plan.save_to_file(&session_file)
        .expect("save R-4 session plan");
    (plan, session_file, rlib)
}

fn write_fixture(workspace: &Path, otel_shim: &Path) -> (PathBuf, PathBuf) {
    fs::write(
        workspace.join("Cargo.toml"),
        r#"[workspace]
members = ["app", "dep_r4", "otel_provider", "probe_macro"]
resolver = "2"
"#,
    )
    .unwrap();

    let dep = workspace.join("dep_r4");
    fs::create_dir_all(dep.join("src")).unwrap();
    fs::write(
        dep.join("Cargo.toml"),
        r#"[package]
name = "dep_r4"
version = "0.1.0"
edition = "2021"

[dependencies]
"#,
    )
    .unwrap();
    fs::write(
        dep.join("src/lib.rs"),
        r#"pub fn sync_work() -> u32 {
    7
}

pub async fn async_work() -> u32 {
    let mut first_poll = true;
    std::future::poll_fn(move |cx| {
        if first_poll {
            first_poll = false;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        } else {
            std::task::Poll::Ready(())
        }
    })
    .await;
    11
}

pub async fn async_result_work(fail: bool) -> Result<u32, String> {
    let mut first_poll = true;
    std::future::poll_fn(move |cx| {
        if first_poll {
            first_poll = false;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        } else {
            std::task::Poll::Ready(())
        }
    })
    .await;
    if fail {
        Err("dependency error".to_string())
    } else {
        Ok(42)
    }
}
"#,
    )
    .unwrap();

    let provider = workspace.join("otel_provider");
    fs::create_dir_all(provider.join("src")).unwrap();
    fs::write(
        provider.join("Cargo.toml"),
        r#"[package]
name = "otel_provider"
version = "0.1.0"
edition = "2021"

[dependencies]
opentelemetry = "0.32.0"
opentelemetry_sdk = { version = "0.32.0", features = ["testing"] }
"#,
    )
    .unwrap();
    fs::write(provider.join("src/lib.rs"), "pub fn ready() {}\n").unwrap();

    let macro_crate = workspace.join("probe_macro");
    fs::create_dir_all(macro_crate.join("src")).unwrap();
    fs::write(
        macro_crate.join("Cargo.toml"),
        r#"[package]
name = "probe_macro"
version = "0.1.0"
edition = "2021"

[lib]
proc-macro = true
"#,
    )
    .unwrap();
    fs::write(
        macro_crate.join("src/lib.rs"),
        r#"use proc_macro::TokenStream;

#[proc_macro_attribute]
pub fn passthrough(_: TokenStream, item: TokenStream) -> TokenStream {
    item
}
"#,
    )
    .unwrap();

    let app = workspace.join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    let otel_shim = otel_shim.to_string_lossy().replace('\\', "/");
    fs::write(
        app.join("Cargo.toml"),
        format!(
            r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
dep_r4 = {{ path = "../dep_r4" }}
opentelemetry = "0.32.0"
opentelemetry_sdk = {{ version = "0.32.0", features = ["testing"] }}
otel-shim = {{ path = "{otel_shim}" }}
probe_macro = {{ path = "../probe_macro" }}
tokio = {{ version = "1", features = ["rt-multi-thread", "macros"] }}
"#
        ),
    )
    .unwrap();
    fs::write(
        app.join("src/main.rs"),
        r#"#![deny(warnings)]

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context as TaskContext, Wake, Waker};
use std::thread;

use opentelemetry::trace::{TraceContextExt as _, Tracer as _};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

struct SimpleWaker(AtomicBool);
impl Wake for SimpleWaker {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn create_waker() -> Waker {
    Waker::from(Arc::new(SimpleWaker(AtomicBool::new(false))))
}

/// Compile-time generic bound asserting that the future itself implements Send.
/// Takes ownership of val to assert Send directly on the future type.
fn assert_is_send<T: Send>(val: T) -> T {
    val
}

fn test_cross_thread_migration<F, R>(
    parent_name: &'static str,
    make_fut: impl FnOnce() -> F + Send + 'static,
) -> (opentelemetry::trace::TraceId, opentelemetry::trace::SpanId)
where
    F: std::future::Future<Output = R> + Send + 'static,
    R: Send + 'static,
{
    let tracer = opentelemetry::global::tracer("r4-app");
    let parent_span = tracer.start(parent_name);
    let parent_cx = opentelemetry::Context::current_with_span(parent_span);
    let parent_trace_id = parent_cx.span().span_context().trace_id();
    let parent_span_id = parent_cx.span().span_context().span_id();

    let (tx_a_to_b, rx_b) = std::sync::mpsc::sync_channel::<Pin<Box<F>>>(1);

    // Thread A: create future under application parent context and poll once to Pending
    let parent_cx_for_a = parent_cx.clone();
    let thread_a = thread::Builder::new()
        .name("thread-a-poller".into())
        .spawn(move || {
            let parent_guard = parent_cx_for_a.attach();

            // Create future under the active application parent context
            let fut = make_fut();
            // Statically assert Send on the future itself
            let fut = assert_is_send(fut);
            let mut pinned_fut = Box::pin(fut);

            // Poll once on Thread A
            let waker_a = create_waker();
            let mut task_cx_a = TaskContext::from_waker(&waker_a);
            let poll1 = pinned_fut.as_mut().poll(&mut task_cx_a);
            assert!(poll1.is_pending(), "first poll on Thread A must return Poll::Pending");

            // Drop parent guard and verify Thread A has no active context leak
            drop(parent_guard);
            assert_eq!(
                opentelemetry::Context::current().span().span_context().span_id(),
                opentelemetry::trace::SpanId::INVALID,
                "Thread A must have no active context leak after first poll and guard drop"
            );

            // Transfer the pending future across thread boundary to Thread B
            tx_a_to_b.send(pinned_fut).expect("send future to Thread B");
        })
        .expect("spawn Thread A");

    thread_a.join().expect("Thread A panicked");

    // Thread B: receive pending future, resume, and poll to Ready
    let thread_b = thread::Builder::new()
        .name("thread-b-poller".into())
        .spawn(move || {
            let mut pinned_fut = rx_b.recv().expect("receive future on Thread B");

            // Verify Thread B does not implicitly inherit any context
            assert_eq!(
                opentelemetry::Context::current().span().span_context().span_id(),
                opentelemetry::trace::SpanId::INVALID,
                "Thread B must have no active context before polling"
            );

            // Poll on Thread B until Poll::Ready
            let waker_b = create_waker();
            let mut task_cx_b = TaskContext::from_waker(&waker_b);
            let poll2 = pinned_fut.as_mut().poll(&mut task_cx_b);
            assert!(poll2.is_ready(), "resumed poll on Thread B must return Poll::Ready");

            // Verify Thread B has no active context leak after completion
            assert_eq!(
                opentelemetry::Context::current().span().span_context().span_id(),
                opentelemetry::trace::SpanId::INVALID,
                "Thread B must have no active context leak after future completion"
            );

            // Drop the completed future on Thread B
            drop(pinned_fut);

            // Verify Thread B remains clean after drop
            assert_eq!(
                opentelemetry::Context::current().span().span_context().span_id(),
                opentelemetry::trace::SpanId::INVALID,
                "Thread B must have no active context leak after dropping future"
            );
        })
        .expect("spawn Thread B");

    thread_b.join().expect("Thread B panicked");

    // End parent span so it exports
    parent_cx.span().end();

    (parent_trace_id, parent_span_id)
}

#[probe_macro::passthrough]
fn main() {
    otel_shim::init();
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    opentelemetry::global::set_tracer_provider(provider);

    // 1. Run direct sync, ordinary async, and tokio::spawn probe
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(run_probe());

    // 2. Run deterministic cross-thread migration test for plain async dependency future
    let (plain_trace_id, plain_parent_id) = test_cross_thread_migration(
        "p24_parent_plain",
        || dep_r4::async_work(),
    );

    // 3. Run deterministic cross-thread migration test for Result async dependency future (error case)
    let (result_trace_id, result_parent_id) = test_cross_thread_migration(
        "p24_parent_result",
        || dep_r4::async_result_work(true),
    );

    let spans = exporter.get_finished_spans().expect("read exported spans");

    // Assert direct sync
    let parent = spans.iter().find(|span| span.name == "r4_parent").expect("parent span");
    let direct_sync = spans.iter().find(|span| span.name == "sync_work").expect("sync dependency span");
    assert_eq!(direct_sync.parent_span_id, parent.span_context.span_id());

    // Assert direct async (ordinary suspension/resumption on runtime)
    let direct_async = spans
        .iter()
        .find(|span| span.name == "async_work" && span.parent_span_id == parent.span_context.span_id())
        .expect("direct async dependency span parented by r4_parent");
    assert_eq!(direct_async.span_context.trace_id(), parent.span_context.trace_id());

    // Assert tokio::spawn context propagation (#6 task-creation boundary)
    let spawned_async = spans
        .iter()
        .find(|span| span.name == "async_work" && span.span_context.span_id() != direct_async.span_context.span_id() && span.parent_span_id != plain_parent_id)
        .expect("spawned async dependency span");
    println!("SPAWN_PARENT={:?}", spawned_async.parent_span_id);
    assert_eq!(
        spawned_async.parent_span_id,
        parent.span_context.span_id(),
        "tokio::spawn must preserve parent SpanId across task boundary via call-site context capture"
    );
    assert_eq!(
        spawned_async.span_context.trace_id(),
        parent.span_context.trace_id(),
        "tokio::spawn must preserve TraceId across task boundary"
    );

    // Assert cross-thread migration for plain async future
    let cross_plain = spans
        .iter()
        .find(|span| span.name == "async_work" && span.parent_span_id == plain_parent_id)
        .expect("cross-thread plain async span parented by p24_parent_plain");
    assert_eq!(cross_plain.span_context.trace_id(), plain_trace_id, "TraceId must match application parent");
    assert_eq!(cross_plain.parent_span_id, plain_parent_id, "Parent SpanId must match application parent");
    assert_eq!(cross_plain.status, opentelemetry::trace::Status::Unset);
    let cross_plain_count = spans
        .iter()
        .filter(|span| span.name == "async_work" && span.parent_span_id == plain_parent_id)
        .count();
    assert_eq!(cross_plain_count, 1, "exactly one plain async span exported across migration");

    // Assert cross-thread migration for Result async future
    let cross_result = spans
        .iter()
        .find(|span| span.name == "async_result_work" && span.parent_span_id == result_parent_id)
        .expect("cross-thread result async span parented by p24_parent_result");
    assert_eq!(cross_result.span_context.trace_id(), result_trace_id, "Result TraceId must match application parent");
    assert_eq!(cross_result.parent_span_id, result_parent_id, "Result Parent SpanId must match application parent");
    assert_eq!(cross_result.status, opentelemetry::trace::Status::error(""), "Result span must capture error status");
    let cross_result_count = spans
        .iter()
        .filter(|span| span.name == "async_result_work" && span.parent_span_id == result_parent_id)
        .count();
    assert_eq!(cross_result_count, 1, "exactly one result async span exported across migration");

    println!("R4_EXTERN_INJECTION_VERIFIED");
    println!("P24_ASYNC_CROSS_THREAD_VERIFIED");
}

async fn run_probe() {
    let tracer = opentelemetry::global::tracer("r4-app");
    let span = tracer.start("r4_parent");
    let cx = opentelemetry::Context::current_with_span(span);
    opentelemetry::trace::FutureExt::with_context(async move {
        assert_eq!(dep_r4::sync_work(), 7);
        assert_eq!(dep_r4::async_work().await, 11);
        let spawned_res = tokio::spawn(async { dep_r4::async_work().await }).await.unwrap();
        assert_eq!(spawned_res, 11);
    }, cx).await;
}
"#,
    )
    .unwrap();

    (dep.join("Cargo.toml"), dep.join("src/lib.rs"))
}

#[test]
#[ignore = "R-4 spike: isolated multi-crate Cargo proof; run explicitly"]
fn r4_extern_injection_path_dependency_proof() {
    let temp = tempfile::tempdir().expect("create R-4 workspace");
    let workspace = temp.path();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let (dep_manifest, dep_source) = write_fixture(workspace, &repo_root.join("otel-shim"));
    let source_before = fs::read(&dep_source).unwrap();
    let manifest_before = fs::read(&dep_manifest).unwrap();
    let registry_source = cached_otel_source();
    let registry_before = snapshot_tree(&registry_source);
    assert!(
        !String::from_utf8_lossy(&manifest_before).contains("opentelemetry"),
        "the dependency fixture must not declare OpenTelemetry"
    );

    let target = workspace.join("target");
    let (_, session_file, rlib) = prepare_r4_session(workspace, &target);

    // A second plausible filename proves the wrapper is not selecting an artifact by globbing.
    let duplicate_name = target.join("debug/deps/libopentelemetry-r4-ambiguous.rlib");
    fs::copy(&rlib, &duplicate_name).expect("create decoy artifact");

    let cold = run_cargo(
        workspace,
        &target,
        &["run", "--package", "app"],
        &session_file,
    );
    assert_success(&cold, "R-4 cold instrumented build");
    assert!(
        String::from_utf8_lossy(&cold.stdout).contains("R4_EXTERN_INJECTION_VERIFIED"),
        "cold run did not verify native dependency spans:\n{}",
        String::from_utf8_lossy(&cold.stdout)
    );
    assert!(
        String::from_utf8_lossy(&cold.stdout).contains("P24_ASYNC_CROSS_THREAD_VERIFIED"),
        "cold run did not verify cross-thread async context propagation:\n{}",
        String::from_utf8_lossy(&cold.stdout)
    );
    assert!(
        !String::from_utf8_lossy(&cold.stderr).contains("crate=probe_macro]"),
        "proc-macro units must remain excluded from the R-4 path"
    );
    let mirrored_dependency = find_mirrored_dependency_source(&target)
        .expect("find R-4 native-instrumented dependency mirror");
    let mirrored_contents = fs::read_to_string(mirrored_dependency).unwrap();
    assert!(
        mirrored_contents.contains("opentelemetry::global::tracer(\"dep_r4\")"),
        "the selected dependency must use native OpenTelemetry output"
    );
    assert!(
        mirrored_contents.contains("opentelemetry::trace::FutureExt::with_context"),
        "the selected dependency async function must retain P1.6 native future instrumentation"
    );
    assert!(
        !mirrored_contents.contains("__otel_span_enter"),
        "the R-4 path must not use the C ABI for the selected dependency"
    );

    let repeat = run_cargo(
        workspace,
        &target,
        &["run", "--package", "app"],
        &session_file,
    );
    assert_success(&repeat, "R-4 repeat build");

    fs::write(
        workspace.join("app/src/main.rs"),
        format!(
            "{}\n// incremental app-only edit\n",
            fs::read_to_string(workspace.join("app/src/main.rs")).unwrap()
        ),
    )
    .unwrap();
    let incremental = run_cargo(
        workspace,
        &target,
        &["run", "--package", "app"],
        &session_file,
    );
    assert_success(&incremental, "R-4 incremental app rebuild");

    let clean = Command::new("cargo")
        .args(["clean", "--target-dir"])
        .arg(&target)
        .current_dir(workspace)
        .output()
        .expect("clean R-4 app artifacts");
    assert_success(&clean, "R-4 clean");
    let (mut clean_plan, clean_session_file, _) = prepare_r4_session(workspace, &target);
    let clean_rebuild = run_cargo(
        workspace,
        &target,
        &["run", "--package", "app"],
        &clean_session_file,
    );
    assert_success(&clean_rebuild, "R-4 clean rebuild");

    let clean_dependency = Command::new("cargo")
        .args(["clean", "--package", "dep_r4", "--target-dir"])
        .arg(&target)
        .current_dir(workspace)
        .output()
        .expect("clean R-4 dependency before missing-artifact probe");
    assert_success(&clean_dependency, "R-4 missing-artifact dependency clean");
    clean_plan.r4_native_otel_artifacts.clear();
    clean_plan
        .save_to_file(&clean_session_file)
        .expect("save missing-artifact R-4 plan");
    let missing = run_cargo(
        workspace,
        &target,
        &["check", "--package", "app"],
        &clean_session_file,
    );
    assert_success(&missing, "R-4 missing-artifact fail-open check");
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("artifact")
            && String::from_utf8_lossy(&missing.stderr).contains("fail-open"),
        "missing artifact must be diagnosed and preserve the existing Tier-2 path:\n{}",
        String::from_utf8_lossy(&missing.stderr)
    );

    assert_eq!(
        source_before,
        fs::read(&dep_source).unwrap(),
        "dependency source mutated"
    );
    assert_eq!(
        manifest_before,
        fs::read(&dep_manifest).unwrap(),
        "dependency manifest mutated"
    );
    assert_eq!(
        registry_before,
        snapshot_tree(&registry_source),
        "Cargo registry source cache mutated"
    );
}

#[test]
fn h1_cli_automatically_acquires_and_injects_native_otel() {
    let temp = tempfile::tempdir().expect("create CLI R-4 workspace");
    let workspace = temp.path();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let (dep_manifest, dep_source) = write_fixture(workspace, &repo_root.join("otel-shim"));
    let source_before = fs::read(&dep_source).unwrap();
    let manifest_before = fs::read(&dep_manifest).unwrap();
    let registry_source = cached_otel_source();
    let registry_before = snapshot_tree(&registry_source);

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("run ordinary cargo-instrument CLI");
    assert_success(&output, "ordinary CLI native R-4 build");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("R4_EXTERN_INJECTION_VERIFIED"),
        "ordinary CLI run did not export native dependency spans:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("crate=dep_r4"),
        "dep_r4 did not pass through RUSTC_WRAPPER:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let target = workspace.join("target/instrumented");
    let session = SessionPlan::load_from_file(&target.join("cargo_instrument_session.json"))
        .expect("CLI must save its per-invocation session plan");
    assert_eq!(
        session.r4_native_otel_artifacts.len(),
        1,
        "CLI must capture one exact artifact"
    );
    assert!(session.r4_native_otel_artifacts[0].rlib_path.is_file());
    let native_extern = format!(
        "injecting --extern opentelemetry={}",
        session.r4_native_otel_artifacts[0].rlib_path.display()
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(&native_extern),
        "the native dependency did not receive the exact Cargo-reported --extern"
    );
    let mirror = find_mirrored_dependency_source(&target).expect("find native dependency mirror");
    let contents = fs::read_to_string(mirror).unwrap();
    assert!(contents.contains("opentelemetry::global::tracer(\"dep_r4\")"));
    assert!(!contents.contains("__otel_span_enter"));
    assert_eq!(
        source_before,
        fs::read(dep_source).unwrap(),
        "dependency source mutated"
    );
    assert_eq!(
        manifest_before,
        fs::read(dep_manifest).unwrap(),
        "dependency manifest mutated"
    );
    assert_eq!(
        registry_before,
        snapshot_tree(&registry_source),
        "registry source cache mutated"
    );
}

fn write_mixed_fixture(workspace: &Path, otel_shim: &Path) {
    fs::write(
        workspace.join("Cargo.toml"),
        r#"[workspace]
members = ["app", "dep_native", "dep_tier2"]
resolver = "2"

[profile.dev.package.dep_tier2]
opt-level = 2
"#,
    )
    .unwrap();

    for (name, fn_name, val) in [
        ("dep_native", "native_work", 7u32),
        ("dep_tier2", "tier2_work", 11u32),
    ] {
        let dir = workspace.join(name);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        )
        .unwrap();
        fs::write(
            dir.join("src/lib.rs"),
            format!("pub fn {fn_name}() -> u32 {{ {val} }}\n"),
        )
        .unwrap();
    }

    let shim = otel_shim.to_string_lossy().replace('\\', "/");
    let app = workspace.join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("Cargo.toml"),
        format!(
            r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
dep_native = {{ path = "../dep_native" }}
dep_tier2 = {{ path = "../dep_tier2" }}
otel-shim = {{ path = "{shim}" }}
opentelemetry = "0.32.0"
opentelemetry_sdk = {{ version = "0.32.0", features = ["testing"] }}
"#
        ),
    )
    .unwrap();
    fs::write(
        app.join("src/main.rs"),
        r#"use opentelemetry::trace::{TraceContextExt as _, Tracer as _};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

fn main() {
    otel_shim::init();
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder().with_simple_exporter(exporter.clone()).build();
    opentelemetry::global::set_tracer_provider(provider);
    let tracer = opentelemetry::global::tracer("mixed-app");
    let parent = tracer.start("mixed_parent");
    let cx = opentelemetry::Context::current_with_span(parent);
    let parent_id = cx.span().span_context().span_id();
    let trace_id = cx.span().span_context().trace_id();
    { let _guard = cx.clone().attach(); assert_eq!(dep_native::native_work(), 7); assert_eq!(dep_tier2::tier2_work(), 11); }
    cx.span().end();
    let spans = exporter.get_finished_spans().unwrap();
    for name in ["native_work", "tier2_work"] {
        let span = spans.iter().find(|span| span.name == name).expect("dependency span");
        assert_eq!(span.parent_span_id, parent_id); assert_eq!(span.span_context.trace_id(), trace_id);
    }
    println!("H1_MIXED_NATIVE_TIER2_VERIFIED");
}
"#,
    )
    .unwrap();
}

#[test]
fn h1_cli_mixed_native_and_tier2_recompile_and_export() {
    let temp = tempfile::tempdir().expect("create mixed H1 workspace");
    let workspace = temp.path();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    write_mixed_fixture(workspace, &repo_root.join("otel-shim"));
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .unwrap();
    assert_success(&output, "mixed native/Tier-2 CLI run");
    assert!(String::from_utf8_lossy(&output.stdout).contains("H1_MIXED_NATIVE_TIER2_VERIFIED"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("crate=dep_native"),
        "native dependency bypassed wrapper:\n{stderr}"
    );
    assert!(
        stderr.contains("crate=dep_tier2"),
        "Tier-2 dependency bypassed wrapper:\n{stderr}"
    );
    assert!(stderr.contains("crate=dep_native] selecting native R-4 emitter"));
    assert!(stderr.contains("crate=dep_tier2] selecting Tier-2 C-ABI emitter"));
    let mirrors = find_mirrored_sources(
        &workspace.join("target/instrumented"),
        "__cargo_instrument_anchor",
    );
    let native = mirrors
        .iter()
        .find(|path| {
            fs::read_to_string(path)
                .unwrap()
                .contains("pub fn native_work")
        })
        .expect("native mirror");
    let tier2 = mirrors
        .iter()
        .find(|path| {
            fs::read_to_string(path)
                .unwrap()
                .contains("pub fn tier2_work")
        })
        .expect("Tier-2 mirror");
    assert!(fs::read_to_string(native)
        .unwrap()
        .contains("opentelemetry::global::tracer(\"dep_native\")"));
    assert!(fs::read_to_string(tier2)
        .unwrap()
        .contains("__otel_span_enter"));
    assert!(!fs::read_to_string(tier2)
        .unwrap()
        .contains("opentelemetry::global::tracer(\"dep_tier2\")"));
}

#[test]
fn h1_cli_failed_prepass_recompiles_dependencies_through_tier2() {
    let temp = tempfile::tempdir().expect("create failed-prepass workspace");
    let workspace = temp.path();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    write_mixed_fixture(workspace, &repo_root.join("otel-shim"));
    let app_source = workspace.join("app/src/main.rs");
    fs::write(
        &app_source,
        "fn main() { let _ = dep_native::native_work(); let _ = dep_tier2::tier2_work(); compile_error!(\"intentional H1 pre-pass failure\"); }\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "build", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "the intentional app error must fail the final build"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("pre-pass exited"),
        "failed pre-pass must be diagnosed:\n{stderr}"
    );
    assert!(
        stderr.contains("crate=dep_native"),
        "dep_native was reused after failed pre-pass:\n{stderr}"
    );
    assert!(
        stderr.contains("crate=dep_tier2"),
        "dep_tier2 was reused after failed pre-pass:\n{stderr}"
    );
    let mirrors = find_mirrored_sources(
        &workspace.join("target/instrumented"),
        "__cargo_instrument_anchor",
    );
    for function in ["pub fn native_work", "pub fn tier2_work"] {
        let mirror = mirrors
            .iter()
            .find(|path| fs::read_to_string(path).unwrap().contains(function))
            .expect("dependency mirror after pre-pass failure");
        assert!(
            fs::read_to_string(mirror)
                .unwrap()
                .contains("__otel_span_enter"),
            "Tier-2 recovery was not applied for {function}"
        );
    }
}

#[test]
fn h1_cli_prepopulated_target_dir_recompiles_uninstrumented_dependencies() {
    let temp = tempfile::tempdir().expect("create prepopulated H1 workspace");
    let workspace = temp.path();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    write_mixed_fixture(workspace, &repo_root.join("otel-shim"));

    let target_dir = workspace.join("target/instrumented");

    // 1. Prepopulate the exact target directory using ordinary, uninstrumented Cargo
    let prepopulate = Command::new("cargo")
        .args(["build", "--package", "app", "--offline", "--target-dir"])
        .arg(&target_dir)
        .current_dir(workspace)
        .output()
        .expect("pre-populate target directory with uninstrumented cargo");
    assert_success(&prepopulate, "uninstrumented cargo prepopulation");

    // Verify uninstrumented artifacts exist
    assert!(target_dir.join("debug").exists());

    // 2. Invoke the normal cargo-instrument CLI into the same target directory
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .unwrap();
    assert_success(
        &output,
        "cargo-instrument with pre-populated target directory",
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("H1_MIXED_NATIVE_TIER2_VERIFIED"));

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Verifies that the intended dependency compilation units actually pass through RUSTC_WRAPPER
    assert!(
        stderr.contains("crate=dep_native"),
        "dep_native did not pass through RUSTC_WRAPPER on pre-populated target dir:\n{stderr}"
    );
    assert!(
        stderr.contains("crate=dep_tier2"),
        "dep_tier2 did not pass through RUSTC_WRAPPER on pre-populated target dir:\n{stderr}"
    );
    // Verifies native and Tier-2 instrumentation are applied correctly
    assert!(stderr.contains("crate=dep_native] selecting native R-4 emitter"));
    assert!(stderr.contains("crate=dep_tier2] selecting Tier-2 C-ABI emitter"));
}

#[test]
fn h1_cli_run_preserves_application_arguments() {
    let temp = tempfile::tempdir().expect("create args H1 workspace");
    let workspace = temp.path();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let (_, _) = write_fixture(workspace, &repo_root.join("otel-shim"));

    // Modify app/src/main.rs inside main() to assert application arguments
    let app_main = workspace.join("app/src/main.rs");
    let original = fs::read_to_string(&app_main).unwrap();
    let modified = original.replace(
        "otel_shim::init();",
        r#"let args: Vec<String> = std::env::args().collect();
    assert!(args.iter().any(|a| a == "--config"), "missing --config in args: {args:?}");
    assert!(args.iter().any(|a| a == "production.toml"), "missing production.toml in args: {args:?}");
    println!("APP_ARGS_VERIFIED");
    otel_shim::init();"#,
    );
    fs::write(&app_main, modified).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args([
            "run",
            "--package",
            "app",
            "--offline",
            "--",
            "--config",
            "production.toml",
            "--flag-value",
        ])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("run cargo-instrument with app args");
    assert_success(&output, "cargo-instrument run with app args");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("APP_ARGS_VERIFIED"),
        "app did not receive arguments:\n{stdout}"
    );
    assert!(stdout.contains("R4_EXTERN_INJECTION_VERIFIED"));
}

#[test]
fn h1_cli_repeat_and_incremental_builds() {
    let temp = tempfile::tempdir().expect("create repeat/incremental H1 workspace");
    let workspace = temp.path();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let (_, _) = write_fixture(workspace, &repo_root.join("otel-shim"));

    // Initial cold CLI run
    let cold = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("cold run");
    assert_success(&cold, "cold build");
    assert!(String::from_utf8_lossy(&cold.stdout).contains("R4_EXTERN_INJECTION_VERIFIED"));

    // Repeat CLI run (cached, nothing rebuilt, spans preserved)
    let repeat = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("repeat run");
    assert_success(&repeat, "repeat build");
    assert!(String::from_utf8_lossy(&repeat.stdout).contains("R4_EXTERN_INJECTION_VERIFIED"));

    // Incremental app edit
    let app_main = workspace.join("app/src/main.rs");
    fs::write(
        &app_main,
        format!(
            "{}\n// incremental edit comment\n",
            fs::read_to_string(&app_main).unwrap()
        ),
    )
    .unwrap();

    let incremental = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("incremental run");
    assert_success(&incremental, "incremental build");
    assert!(String::from_utf8_lossy(&incremental.stdout).contains("R4_EXTERN_INJECTION_VERIFIED"));
}

#[test]
fn h1_cli_clean_rebuild_and_automatic_artifact_reacquisition() {
    let temp = tempfile::tempdir().expect("create clean-rebuild H1 workspace");
    let workspace = temp.path();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let (_, _) = write_fixture(workspace, &repo_root.join("otel-shim"));

    // Initial cold CLI run
    let cold = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--package", "app", "--offline"])
        .current_dir(workspace)
        .output()
        .expect("cold run");
    assert_success(&cold, "cold build");

    // Perform cargo clean
    let clean = Command::new("cargo")
        .args(["clean", "--target-dir", "target/instrumented"])
        .current_dir(workspace)
        .output()
        .expect("cargo clean");
    assert_success(&clean, "cargo clean");

    // Rebuild after clean — proves automatic artifact reacquisition
    let rebuilt = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("clean rebuild");
    assert_success(&rebuilt, "rebuilt after clean");
    assert!(String::from_utf8_lossy(&rebuilt.stdout).contains("R4_EXTERN_INJECTION_VERIFIED"));
}

#[test]
fn h1_cli_selective_clean_failure_terminates_with_diagnostic() {
    let temp = tempfile::tempdir().expect("create clean failure H1 workspace");
    let workspace = temp.path();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let (_, _) = write_fixture(workspace, &repo_root.join("otel-shim"));

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("__CARGO_INSTRUMENT_FAULT_INJECT_CLEAN_FAIL", "1")
        .output()
        .expect("run with clean fault injection");
    assert!(
        !output.status.success(),
        "clean failure must terminate orchestration non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cargo-instrument orchestration error")
            && stderr.contains("cargo clean failed"),
        "missing actionable selective invalidation diagnostic:\n{stderr}"
    );
}

#[test]
fn h1_cli_ambiguous_package_name_prevents_unsafe_clean() {
    let temp = tempfile::tempdir().expect("create ambiguous H1 workspace");
    let workspace = temp.path();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let shim = repo_root
        .join("otel-shim")
        .to_string_lossy()
        .replace('\\', "/");

    // Two external path dependencies outside workspace members share package name "ambiguous_dep"
    fs::write(
        workspace.join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"decoy\"]\nexclude = [\"ambiguous_dep_v1\", \"ambiguous_dep_v2\"]\nresolver = \"2\"\n",
    ).unwrap();

    let dir_v1 = workspace.join("ambiguous_dep_v1");
    fs::create_dir_all(dir_v1.join("src")).unwrap();
    fs::write(
        dir_v1.join("Cargo.toml"),
        "[package]\nname = \"ambiguous_dep\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(dir_v1.join("src/lib.rs"), "pub fn work() -> u32 { 1 }\n").unwrap();

    let dir_v2 = workspace.join("ambiguous_dep_v2");
    fs::create_dir_all(dir_v2.join("src")).unwrap();
    fs::write(
        dir_v2.join("Cargo.toml"),
        "[package]\nname = \"ambiguous_dep\"\nversion = \"0.2.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(dir_v2.join("src/lib.rs"), "pub fn work() -> u32 { 2 }\n").unwrap();

    let decoy = workspace.join("decoy");
    fs::create_dir_all(decoy.join("src")).unwrap();
    fs::write(
        decoy.join("Cargo.toml"),
        r#"[package]
name = "decoy"
version = "0.1.0"
edition = "2021"

[dependencies]
ambiguous_dep = { path = "../ambiguous_dep_v2" }
"#,
    )
    .unwrap();
    fs::write(decoy.join("src/lib.rs"), "pub fn decoy() {}\n").unwrap();

    let app = workspace.join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("Cargo.toml"),
        format!(
            r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
ambiguous_dep = {{ path = "../ambiguous_dep_v1" }}
otel-shim = {{ path = "{shim}" }}
opentelemetry = "0.32.0"
opentelemetry_sdk = {{ version = "0.32.0", features = ["testing"] }}
"#
        ),
    )
    .unwrap();
    fs::write(
        app.join("src/main.rs"),
        "fn main() { otel_shim::init(); assert_eq!(ambiguous_dep::work(), 1); }\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--package", "app", "--offline"])
        .current_dir(workspace)
        .output()
        .expect("run with ambiguous packages");

    // Safe selective clean cannot be established because ambiguous versions exist and only one is selected
    assert!(
        !output.status.success(),
        "ambiguous package invalidation must terminate non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot safely selectively invalidate")
            || stderr.contains("multiple versions exist"),
        "unexpected error message:\n{stderr}"
    );
}
