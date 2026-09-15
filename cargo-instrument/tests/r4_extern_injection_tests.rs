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

use opentelemetry::trace::{TraceContextExt as _, Tracer as _};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

#[probe_macro::passthrough]
fn main() {
    otel_shim::init();
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    opentelemetry::global::set_tracer_provider(provider);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(run_probe());

    let spans = exporter.get_finished_spans().expect("read exported spans");
    let parent = spans.iter().find(|span| span.name == "r4_parent").expect("parent span");
    let direct_sync = spans.iter().find(|span| span.name == "sync_work").expect("sync dependency span");
    assert_eq!(direct_sync.parent_span_id, parent.span_context.span_id());

    let direct_async = spans
        .iter()
        .find(|span| span.name == "async_work" && span.parent_span_id == parent.span_context.span_id())
        .expect("direct async dependency span parented by r4_parent");
    let spawned_async = spans
        .iter()
        .find(|span| span.name == "async_work" && span.span_context.span_id() != direct_async.span_context.span_id())
        .expect("spawned async dependency span");
    println!("SPAWN_PARENT={:?}", spawned_async.parent_span_id);
    assert_eq!(
        spawned_async.parent_span_id,
        opentelemetry::trace::SpanId::INVALID,
        "native FutureExt propagation alone must not claim to preserve context across tokio::spawn"
    );
    println!("R4_EXTERN_INJECTION_VERIFIED");
}

async fn run_probe() {
    let tracer = opentelemetry::global::tracer("r4-app");
    let span = tracer.start("r4_parent");
    let cx = opentelemetry::Context::current_with_span(span);
    opentelemetry::trace::FutureExt::with_context(async move {
        assert_eq!(dep_r4::sync_work(), 7);
        assert_eq!(dep_r4::async_work().await, 11);
        assert_eq!(tokio::spawn(async { dep_r4::async_work().await }).await.unwrap(), 11);
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
