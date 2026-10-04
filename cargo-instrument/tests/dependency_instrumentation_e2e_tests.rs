use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root).unwrap().flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|name| name == "target") {
            continue;
        }
        if path.is_dir() {
            files.extend(snapshot(&path));
        } else {
            files.push((path.clone(), fs::read(path).unwrap()));
        }
    }
    files.sort();
    files
}

fn cli(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(args)
        .current_dir(root)
        .env("CARGO_NET_OFFLINE", "true")
        .env("INSTRUMENT_DEBUG", "1")
        .env_remove("CARGO_INSTRUMENT_DEPENDENCIES")
        .env_remove("CARGO_INSTRUMENT_REGISTRY")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("CARGO_INSTRUMENT_SESSION")
        .output()
        .unwrap()
}

#[test]
fn tokio_metadata_identity_uses_only_cargo_reported_companion_paths() {
    use cargo_instrument::session::{CargoDepEdge, SessionPlan};
    use std::collections::HashMap;
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("src/lib.rs");
    let rlib = temp.path().join("libtokio.rlib");
    let metadata = temp.path().join("libtokio.rmeta");
    fs::write(&rlib, b"rlib").unwrap();
    fs::write(&metadata, b"rmeta").unwrap();
    let mut plan = SessionPlan {
        package_manifest_dirs: HashMap::from([("app".into(), temp.path().to_path_buf())]),
        package_names_by_id: HashMap::from([("tokio-id".into(), "tokio".into())]),
        package_dependencies: HashMap::from([(
            "app".into(),
            vec![CargoDepEdge {
                binding_name: "tokio".into(),
                package_id: "tokio-id".into(),
                kinds: vec![None],
            }],
        )]),
        ..Default::default()
    };
    let messages = serde_json::json!({"reason":"compiler-artifact", "package_id":"tokio-id", "filenames":[rlib, metadata], "features":["rt"], "profile":{"opt_level":"0", "debug_assertions":true,"overflow_checks":true,"test":false}}).to_string();
    let cargo_metadata =
        serde_json::json!({"packages":[{"id":"tokio-id","name":"tokio","version":"1.0.0"}]});
    plan.add_tokio_artifacts_from_cargo_json(&cargo_metadata, messages.as_bytes(), None)
        .unwrap();
    let args = vec!["--extern".into(), format!("tokio={}", metadata.display())];
    assert!(plan.tokio_artifact_for(&source, &args).unwrap().is_some());
    let decoy = temp.path().join("decoy/libtokio.rmeta");
    fs::create_dir_all(decoy.parent().unwrap()).unwrap();
    fs::write(&decoy, b"decoy").unwrap();
    assert!(plan
        .tokio_artifact_for(
            &source,
            &["--extern".into(), format!("tokio={}", decoy.display())]
        )
        .is_err());
    fs::remove_file(metadata).unwrap();
    assert!(plan.tokio_artifact_for(&source, &args).is_err());
    assert!(plan
        .tokio_artifact_for(&source, &["--extern".into(), "tokio".into()])
        .is_err());
    let otel_rlib = temp.path().join("libotel.rlib");
    let otel_meta = temp.path().join("libotel.rmeta");
    fs::write(&otel_rlib, b"otel rlib").unwrap();
    fs::write(&otel_meta, b"otel metadata").unwrap();
    plan.r4_otel_package_by_dependency
        .insert("app".into(), "otel-id".into());
    let messages = serde_json::json!({"reason":"compiler-artifact", "package_id":"otel-id", "filenames":[otel_rlib, otel_meta], "features":["trace"], "profile":{"opt_level":"0", "debug_assertions":true,"overflow_checks":true,"test":false}}).to_string();
    plan.add_r4_artifacts_from_cargo_json(&serde_json::json!({"packages":[{"id":"otel-id","name":"opentelemetry","version":"0.32.0"}]}), messages.as_bytes(), None).unwrap();
    assert!(plan
        .r4_native_otel_artifact_for(
            &source,
            &[
                "--extern".into(),
                format!("opentelemetry={}", otel_meta.display())
            ]
        )
        .unwrap()
        .is_some());
    assert!(plan
        .r4_native_otel_artifact_for(&source, &["--extern".into(), "opentelemetry".into()])
        .is_err());
}

#[test]
fn public_opt_in_fails_open_and_policy_transitions_rebuild_owned_source() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("dep/src")).unwrap();
    fs::write(root.join("Cargo.toml"), "[package]\nname=\"plain-app\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[workspace]\nexclude=[\"dep\"]\n[dependencies]\nplain-dep={path=\"dep\"}\n").unwrap();
    fs::write(
        root.join("src/main.rs"),
        "fn main() { assert_eq!(plain_dep::work(), 7); println!(\"PLAIN_OK\"); }\n",
    )
    .unwrap();
    fs::write(
        root.join("dep/Cargo.toml"),
        "[package]\nname=\"plain-dep\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[workspace]\n",
    )
    .unwrap();
    fs::write(root.join("dep/src/lib.rs"), "pub fn work() -> u32 { 7 }\n").unwrap();
    let output = cli(root, &["--", "run", "--offline"]);
    success(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("transformed 1 candidates"));
    let before = snapshot(root);
    for fault in [
        None,
        Some("__CARGO_INSTRUMENT_FAULT_INJECT_CORRUPT_JSON"),
        Some("__CARGO_INSTRUMENT_FAULT_INJECT_PREPASS_FAIL"),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"));
        command
            .args(["--with-dependencies", "--", "run", "--offline"])
            .current_dir(root)
            .env("INSTRUMENT_DEBUG", "1")
            .env("CARGO_NET_OFFLINE", "true");
        if let Some(fault) = fault {
            command.env(fault, "1");
        }
        let output = command.output().unwrap();
        success(&output);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("crate=plain_app] transformed"));
        assert!(stderr.contains("no otel-shim provider"));
        assert!(String::from_utf8_lossy(&output.stdout).contains("PLAIN_OK"));
        assert_eq!(before, snapshot(root));
    }
    let output = cli(root, &["--", "run", "--offline"]);
    success(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("transformed 1 candidates"));
    let output = cli(
        root,
        &[
            "--with-dependencies",
            "--",
            "run",
            "--offline",
            "--",
            "--help",
        ],
    );
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("PLAIN_OK"));
}

#[test]
fn public_tier2_fallback_exports_versioned_scope_and_source_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("dep/src")).unwrap();
    let shim = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("otel-shim")
        .to_string_lossy()
        .replace('\\', "/");
    fs::write(root.join("Cargo.toml"), format!("[package]\nname=\"metadata-app\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[workspace]\nexclude=[\"dep\"]\n[profile.dev.package.metadata-dep]\nopt-level=1\n[dependencies]\nmetadata-dep={{path=\"dep\"}}\notel-shim={{path=\"{shim}\"}}\nopentelemetry=\"0.32.0\"\nopentelemetry_sdk={{version=\"0.32.0\",features=[\"testing\"]}}\n")).unwrap();
    fs::write(
        root.join("dep/Cargo.toml"),
        "[package]\nname=\"metadata-dep\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[workspace]\n",
    )
    .unwrap();
    fs::write(
        root.join("dep/src/lib.rs"),
        "#![deny(warnings)]\npub fn metadata_work() -> u32 { 11 }\n",
    )
    .unwrap();
    fs::write(root.join("src/main.rs"), r#"#![deny(warnings)]
use opentelemetry::trace::{TraceContextExt as _, Tracer as _};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
fn main() {
    otel_shim::init();
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder().with_simple_exporter(exporter.clone()).build();
    opentelemetry::global::set_tracer_provider(provider);
    let cx = opentelemetry::Context::current_with_span(opentelemetry::global::tracer("manual").start("parent"));
    { let _guard = cx.clone().attach(); assert_eq!(metadata_dep::metadata_work(), 11); }
    assert_eq!(otel_shim::active_span_count(), 0);
    let spans = exporter.get_finished_spans().unwrap();
    let span = spans.iter().find(|s| s.name == "metadata_work").unwrap();
    assert_eq!(span.instrumentation_scope.name(), "metadata_dep");
    assert_eq!(span.parent_span_id, cx.span().span_context().span_id());
    assert!(span.attributes.iter().any(|a| a.key.as_str() == "code.function.name" && a.value.as_str() == "metadata_work"));
    assert!(span.attributes.iter().any(|a| a.key.as_str() == "code.line.number" && a.value == opentelemetry::Value::I64(2)));
    assert!(span.attributes.iter().any(|a| a.key.as_str() == "code.file.path" && a.value.as_str().replace('\\', "/").ends_with("dep/src/lib.rs")));
    println!("METADATA_V2_OK");
}
"#).unwrap();
    let output = cli(root, &["--with-dependencies", "--", "run", "--offline"]);
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("METADATA_V2_OK"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("selecting Tier-2 C-ABI emitter"));
}

#[test]
fn polling_adapters_are_excluded_and_full_second_pass_is_idempotent() {
    use cargo_instrument::{
        ast::analyze_source_str,
        transform::{NativeOtelEmitter, TransformationPlan},
    };
    let source = r#"
impl futures_core::Stream for Feed { fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<u8>> { Poll::Pending } }
impl futures_sink::Sink<u8> for Feed {
    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), ()>> { Poll::Pending }
    fn start_send(self: Pin<&mut Self>, _: u8) -> Result<(), ()> { Ok(()) }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), ()>> { Poll::Pending }
    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), ()>> { Poll::Pending }
}
impl core::future::Future for Feed { fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> { Poll::Pending } }
fn poll_next() -> u8 { 7 }
pub async fn work() { tokio::spawn(async { 1 }).await.unwrap(); }
"#;
    let path = Path::new("src/lib.rs");
    let first = analyze_source_str("dep", path, source).unwrap();
    assert_eq!(first.skipped_stats.adapter_trait, 6);
    assert_eq!(first.candidates.len(), 2);
    let emitter = NativeOtelEmitter::for_dependency("dep");
    let transformed = TransformationPlan::build_with_emitter_and_spawns(
        source,
        &first.candidates,
        &first.spawn_sites,
        &emitter,
    )
    .unwrap()
    .apply(source)
    .unwrap();
    let second = analyze_source_str("dep", path, &transformed).unwrap();
    assert!(second.candidates.is_empty());
    assert!(second.spawn_sites.is_empty());
    let repeated = TransformationPlan::build_with_emitter_and_spawns(
        &transformed,
        &second.candidates,
        &second.spawn_sites,
        &emitter,
    )
    .unwrap()
    .apply(&transformed)
    .unwrap();
    assert_eq!(transformed, repeated);
}

#[test]
fn public_opt_in_cli_proves_dependency_lifecycle_registry_and_build_cycles() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir_all(root.join("app/src")).unwrap();
    fs::create_dir_all(root.join("dep/src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers=[\"app\"]\nexclude=[\"dep\"]\nresolver=\"2\"\n",
    )
    .unwrap();
    fs::write(root.join("dep/Cargo.toml"), "[package]\nname=\"p24-dep\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[dependencies]\ntokio={version=\"1\",features=[\"rt\",\"sync\"]}\n").unwrap();
    fs::write(root.join("dep/src/lib.rs"), DEPENDENCY).unwrap();
    fs::write(root.join("app/Cargo.toml"), "[package]\nname=\"p24-app\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[dependencies]\np24-dep={path=\"../dep\"}\ncensus=\"=0.4.2\"\nopentelemetry=\"=0.32.0\"\nopentelemetry_sdk={version=\"0.32.0\",features=[\"testing\"]}\ntokio={version=\"1\",features=[\"rt-multi-thread\",\"macros\",\"sync\"]}\n").unwrap();
    fs::write(root.join("app/src/main.rs"), APPLICATION).unwrap();
    // Resolve the lockfile once, before the immutable-input baseline.
    success(
        &Command::new("cargo")
            .args(["generate-lockfile", "--offline"])
            .current_dir(root)
            .output()
            .unwrap(),
    );
    let original = snapshot(root);
    for cycle in [
        "cold",
        "repeat",
        "incremental-app",
        "incremental-dep",
        "clean",
    ] {
        if cycle == "incremental-app" {
            fs::write(
                root.join("app/src/main.rs"),
                format!("{APPLICATION}\n// app change\n"),
            )
            .unwrap();
        }
        if cycle == "incremental-dep" {
            fs::write(
                root.join("dep/src/lib.rs"),
                format!("{DEPENDENCY}\n// dep change\n"),
            )
            .unwrap();
        }
        if cycle == "clean" {
            success(
                &Command::new("cargo")
                    .args(["clean", "--target-dir", "target/instrumented", "--offline"])
                    .current_dir(root)
                    .output()
                    .unwrap(),
            );
        }
        let before = snapshot(root);
        let output = cli(
            root,
            &[
                "--with-dependencies",
                "--",
                "run",
                "--offline",
                "--",
                "--with-dependencies",
            ],
        );
        success(&output);
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("P24_COMPLETE"),
            "{cycle}"
        );
        assert_eq!(before, snapshot(root), "inputs changed during {cycle}");
        let plan: cargo_instrument::SessionPlan = serde_json::from_slice(
            &fs::read(root.join("target/instrumented/cargo_instrument_session.json")).unwrap(),
        )
        .unwrap();
        let app_id = plan
            .package_names_by_id
            .iter()
            .find(|(_, name)| *name == "p24-app")
            .unwrap()
            .0;
        assert!(plan.wrapper_excluded_package_ids.contains(app_id));
        assert!(
            !plan.r4_native_otel_artifacts.is_empty(),
            "public opt-in must reach native R-4"
        );
    }
    assert!(original
        .iter()
        .all(|(path, bytes)| path.ends_with("src/main.rs")
            || path.ends_with("src/lib.rs")
            || fs::read(path).unwrap() == *bytes));
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--offline", "--", "--with-dependencies"])
        .env("CARGO_INSTRUMENT_DEPENDENCIES", "1")
        .env("CARGO_NET_OFFLINE", "true")
        .current_dir(root)
        .output()
        .unwrap();
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("P24_COMPLETE"));
}

const DEPENDENCY: &str = r#"#![deny(warnings)]
pub fn sync_work() -> u32 { 42 }
pub async fn complete() -> u32 { tokio::task::yield_now().await; 9 }
pub async fn error() -> Result<(), ()> { Err(()) }
pub async fn early() -> Result<u32, ()> { return Ok(7); }
pub async fn aborted(ready: tokio::sync::oneshot::Sender<()>) { let _ = ready.send(()); core::future::pending::<()>().await; }
pub async fn dropped() { core::future::pending::<()>().await; }
pub async fn selected() { core::future::pending::<()>().await; }
pub async fn unpolled() { core::future::pending::<()>().await; }
pub async fn panics() { panic!("expected dependency panic"); }
pub async fn spawn_child() -> u32 { tokio::spawn(async { complete().await }).await.unwrap() }
"#;

const APPLICATION: &str = r#"#![deny(warnings)]
use opentelemetry::trace::{TraceContextExt as _, Tracer as _, FutureExt as _, Status, SpanId, TraceId};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

#[tokio::main(flavor="multi_thread", worker_threads=2)]
async fn main() {
    assert_eq!(std::env::args().nth(1).as_deref(), Some("--with-dependencies"));
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder().with_simple_exporter(exporter.clone()).build();
    opentelemetry::global::set_tracer_provider(provider.clone());
    let tracer = opentelemetry::global::tracer("manual");
    let parent = opentelemetry::Context::current_with_span(tracer.start("parent"));
    let trace = parent.span().span_context().trace_id();
    let parent_id = parent.span().span_context().span_id();
    assert_ne!(trace, TraceId::INVALID);
    assert_ne!(parent_id, SpanId::INVALID);
    async {
        assert_eq!(p24_dep::sync_work(), 42);
        assert_eq!(p24_dep::complete().await, 9);
        assert!(p24_dep::error().await.is_err());
        assert_eq!(p24_dep::early().await, Ok(7));
        assert_eq!(p24_dep::spawn_child().await, 9);
        drop(p24_dep::unpolled());
        let (tx, rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(p24_dep::aborted(tx).with_context(opentelemetry::Context::current()));
        rx.await.unwrap();
        handle.abort();
        assert!(handle.await.unwrap_err().is_cancelled());
        let mut future = Box::pin(p24_dep::dropped());
        let mut task_cx = core::task::Context::from_waker(core::task::Waker::noop());
        assert!(core::future::Future::poll(future.as_mut(), &mut task_cx).is_pending());
        drop(future);
        tokio::select! { biased; _ = p24_dep::selected() => panic!("pending became ready"), _ = core::future::ready(()) => {} }
        let inventory = census::Inventory::<u32>::new();
        let guard = inventory.track(123);
        assert_eq!(inventory.list().len(), 1);
        drop(guard);
        let panic_task = tokio::spawn(p24_dep::panics().with_context(opentelemetry::Context::current()));
        assert!(panic_task.await.unwrap_err().is_panic());
    }.with_context(parent.clone()).await;
    assert_eq!(opentelemetry::Context::current().span().span_context().span_id(), SpanId::INVALID);
    parent.span().end();
    let spans = exporter.get_finished_spans().unwrap();
    let outcome = |name: &str, expected: &str, count: usize| {
        let found: Vec<_> = spans.iter().filter(|s| s.name == name).collect();
        assert_eq!(found.len(), count, "span count for {name}: {spans:?}");
        for span in found {
            assert_eq!(span.span_context.trace_id(), trace, "trace for {name}: {spans:?}");
            assert!(span.attributes.iter().any(|a| a.key.as_str() == "cargo.instrumentation.async.outcome" && a.value.as_str() == expected), "outcome for {name}");
        }
    };
    outcome("complete", "completed", 2);
    outcome("early", "completed", 1);
    outcome("error", "completed", 1);
    outcome("spawn_child", "completed", 1);
    outcome("aborted", "cancelled", 1);
    outcome("dropped", "cancelled", 1);
    outcome("selected", "cancelled", 1);
    outcome("panics", "unwound", 1);
    assert!(!spans.iter().any(|s| s.name == "unpolled"));
    let err = spans.iter().find(|s| s.name == "error").unwrap();
    assert_eq!(err.status, Status::error(""));
    for name in ["aborted", "dropped", "panics", "early"] {
        assert_eq!(spans.iter().find(|s| s.name == name).unwrap().status, Status::Unset);
    }
    let sync = spans.iter().find(|s| s.name == "sync_work").unwrap();
    assert_eq!(sync.parent_span_id, parent_id);
    let spawn = spans.iter().find(|s| s.name == "spawn_child").unwrap();
    assert!(spans.iter().any(|s| s.name == "complete" && s.parent_span_id == spawn.span_context.span_id()));
    assert!(spans.iter().any(|s| s.instrumentation_scope.name() == "census"));
    assert_eq!(spans.iter().filter(|s| s.name == "parent").count(), 1);
    assert!(!spans.iter().any(|s| s.instrumentation_scope.name() == "p24_app"));
    println!("P24_COMPLETE");
    provider.shutdown().unwrap();
}
"#;

// ---------------------------------------------------------------------------
// Phase 3 coverage and async-correctness baseline fixture.
//
// This fixture extends this file's existing infrastructure (cli/success/
// snapshot helpers and the public `cargo instrument --with-dependencies`
// workflow) with a small topology whose expectations are written down
// independently of analyzer output.
// ---------------------------------------------------------------------------

/// Independent static expectation table for [`P31_DEP`] and
/// [`P31_APPLICATION`], derived from those fixture sources by inspection —
/// never from the analyzer's candidate list. Schema:
/// (function, ownership, form, expectation, workload-exercises-it).
///
/// Definition-level counts only: invocation counts live in the runtime
/// oracle inside [`P31_APPLICATION`], and Cargo units (one `p31_dep` lib
/// unit, one `p31_app` bin unit) are asserted separately from packages.
const P31_EXPECTED: &[(&str, &str, &str, &str, bool)] = &[
    ("leaf", "dependency", "sync", "instrumented", true),
    ("mid", "dependency", "sync", "instrumented", true),
    ("outer", "dependency", "sync", "instrumented", true),
    ("async_chain", "dependency", "async", "instrumented", true),
    ("suspended", "dependency", "async", "instrumented", true),
    (
        "spawn_child",
        "dependency",
        "async spawn site",
        "instrumented",
        true,
    ),
    (
        "unsupported_spawn",
        "dependency",
        "async (bare spawn inside)",
        "instrumented",
        true,
    ),
    ("rendezvous", "dependency", "async", "instrumented", true),
    (
        "cancelled_work",
        "dependency",
        "async",
        "instrumented",
        true,
    ),
    ("never_polled", "dependency", "async", "instrumented", false),
    (
        "inlined",
        "dependency",
        "sync",
        "excluded: inline attribute",
        true,
    ),
    (
        "fib",
        "dependency",
        "sync",
        "excluded: direct self recursion",
        true,
    ),
    (
        "app_helper",
        "application",
        "sync",
        "excluded: first-party source outside the dependency workflow",
        true,
    ),
];

#[test]
fn phase3_coverage_baseline_static_expectations_and_runtime_trace_oracle() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir_all(root.join("app/src")).unwrap();
    fs::create_dir_all(root.join("dep/src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers=[\"app\"]\nexclude=[\"dep\"]\nresolver=\"2\"\n",
    )
    .unwrap();
    fs::write(
        root.join("dep/Cargo.toml"),
        "[package]\nname=\"p31-dep\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[dependencies]\ntokio={version=\"1\",features=[\"rt\",\"sync\"]}\n",
    )
    .unwrap();
    fs::write(root.join("dep/src/lib.rs"), P31_DEP).unwrap();
    fs::write(
        root.join("app/Cargo.toml"),
        "[package]\nname=\"p31-app\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[dependencies]\np31-dep={path=\"../dep\"}\nopentelemetry=\"=0.32.0\"\nopentelemetry_sdk={version=\"0.32.0\",features=[\"testing\"]}\ntokio={version=\"1\",features=[\"rt-multi-thread\",\"macros\",\"sync\"]}\n",
    )
    .unwrap();
    fs::write(root.join("app/src/main.rs"), P31_APPLICATION).unwrap();
    // Resolve the lockfile once, before the immutable-input baseline.
    success(
        &Command::new("cargo")
            .args(["generate-lockfile", "--offline"])
            .current_dir(root)
            .output()
            .unwrap(),
    );
    let before = snapshot(root);

    // Exercise the actual generated code through the public dependency workflow.
    let output = cli(root, &["--with-dependencies", "--", "run", "--offline"]);
    success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("P31_COMPLETE"),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert_eq!(before, snapshot(root), "fixture inputs changed during run");

    // --- Static coverage against the independent table (Cargo unit level). ---
    let expected_candidates = P31_EXPECTED
        .iter()
        .filter(|entry| entry.3 == "instrumented")
        .count();
    let dep_transform_lines: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("crate=p31_dep] transformed"))
        .collect();
    assert!(
        !dep_transform_lines.is_empty(),
        "no p31_dep transform line in stderr:\n{stderr}"
    );
    for line in &dep_transform_lines {
        assert!(
            line.contains(&format!("transformed {expected_candidates} candidates")),
            "p31_dep transform count contradicts the independent table: {line}"
        );
    }
    // The workspace application unit must not be touched by the dependency workflow.
    assert!(
        !stderr.contains("crate=p31_app] transformed"),
        "first-party application must not be mirrored:\n{stderr}"
    );
    assert!(
        !stderr.contains("crate=p31_app] selecting"),
        "first-party application must not select an emitter:\n{stderr}"
    );
    // Emitter route must be the native R-4 path, not the C-ABI fallback.
    assert!(
        stderr.contains("crate=p31_dep] selecting native R-4 emitter"),
        "expected native R-4 emission for p31_dep:\n{stderr}"
    );
    assert!(
        !stderr.contains("selecting Tier-2"),
        "C-ABI fallback must not be selected for this fixture:\n{stderr}"
    );

    // --- Generated edits verified against the table (dependency mirror). ---
    let mirror_line = dep_transform_lines.first().expect("p31_dep transform line");
    let mirror_dir = PathBuf::from(
        mirror_line
            .split("into mirror ")
            .nth(1)
            .expect("mirror path in transform line")
            .trim(),
    );
    assert!(
        mirror_dir.is_dir(),
        "mirror dir missing: {}",
        mirror_dir.display()
    );
    fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let mut mirror_sources = Vec::new();
    collect_rs(&mirror_dir, &mut mirror_sources);
    assert_eq!(
        mirror_sources.len(),
        1,
        "expected exactly one mirrored source file under {}",
        mirror_dir.display()
    );
    let mirrored = fs::read_to_string(&mirror_sources[0]).expect("mirrored p31_dep source");
    for (function, ownership, _, expectation, _) in P31_EXPECTED {
        if *ownership != "dependency" {
            continue;
        }
        let anchor = format!("span_builder(&__otel_tracer, \"{function}\")");
        if *expectation == "instrumented" {
            assert!(
                mirrored.contains(&anchor),
                "expected generated span for {function} in mirror:\n{mirrored}"
            );
        } else {
            assert!(
                !mirrored.contains(&anchor),
                "excluded function {function} must not be instrumented in mirror:\n{mirrored}"
            );
        }
    }
    // Supported qualified tokio::spawn site is rewritten...
    assert!(
        mirrored.contains("tokio::spawn(opentelemetry::trace::FutureExt::with_context("),
        "supported spawn site must be rewritten in mirror:\n{mirrored}"
    );
    // ...while the unsupported bare-import spawn site is preserved unrewritten.
    assert!(
        mirrored.contains("bare_spawn(async { leaf(30) })"),
        "unsupported spawn site must remain verbatim in mirror:\n{mirrored}"
    );
    assert!(
        !mirrored.contains("bare_spawn(opentelemetry"),
        "unsupported spawn site must not be rewritten in mirror:\n{mirrored}"
    );
}

/// Unowned path dependency used by the Phase 3 baseline: synchronous nested
/// chain, deterministic async suspension, two spawn forms (one supported, one
/// explicitly unsupported), an interleaving rendezvous, lifecycle cases, and
/// two intentional exclusions.
const P31_DEP: &str = r#"#![deny(warnings)]

// Synchronous nested chain: outer -> mid -> leaf.
pub fn leaf(value: u64) -> u64 { value.wrapping_add(1) }
pub fn mid(value: u64) -> u64 { leaf(value).wrapping_add(1) }
pub fn outer(value: u64) -> u64 { mid(value).wrapping_mul(2) }

// Intentional exclusions (still exercised by the workload).
#[inline]
pub fn inlined(value: u64) -> u64 { value ^ 0xff }
pub fn fib(n: u64) -> u64 { if n <= 1 { n } else { fib(n - 1) + fib(n - 2) } }

// Async with a deterministic suspension point (yield, never a sleep).
pub async fn suspended(value: u64) -> u64 { tokio::task::yield_now().await; leaf(value) }
pub async fn async_chain(value: u64) -> u64 { suspended(value).await }

// Supported qualified spawn site (rewritten by the dependency workflow).
pub async fn spawn_child() -> u64 { tokio::spawn(async { suspended(21).await }).await.unwrap() }

// Unsupported bare-import spawn site: deliberately not rewritten, so the
// dispatched task starts without the caller's context.
use tokio::spawn as bare_spawn;
pub async fn unsupported_spawn() -> u64 { bare_spawn(async { leaf(30) }).await.unwrap() }

// Rendezvous: both request roots must arrive before either may proceed.
pub async fn rendezvous(out: tokio::sync::oneshot::Sender<()>, gate: tokio::sync::oneshot::Receiver<()>) -> u64 {
    let _ = out.send(());
    let _ = gate.await;
    leaf(7)
}

// Lifecycle cases: cancelled after first poll, and never polled at all.
pub async fn cancelled_work() -> u64 { core::future::pending::<u64>().await }
pub async fn never_polled() -> u64 { tokio::task::yield_now().await; 999 }
"#;

/// Application for the Phase 3 baseline. Handwritten OTel SDK setup and two
/// manual request roots are allowed here; all parent-child expectations for
/// generated spans are asserted against spans produced by the actual
/// dependency workflow, with no manual context propagation inside generated
/// code paths.
///
/// Expected runtime inventory (defined from the sources above, independent of
/// discovery output), total 21 finished spans:
/// request_a 1, request_b 1 (manual roots, scope `app_root`); outer 2, mid 2,
/// leaf 7 (6 in-tree + 1 inside the unsupported bare-spawn task), async_chain
/// 1, suspended 2, spawn_child 1, unsupported_spawn 1, rendezvous 2,
/// cancelled_work 1, never_polled 0, inlined 0, fib 0, app_helper 0 (scope
/// `p31_dep` for dependency spans; no automatic first-party spans).
const P31_APPLICATION: &str = r#"#![deny(warnings)]
use opentelemetry::trace::{FutureExt as _, SpanId, TraceContextExt as _, Tracer as _, TraceId};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

// Handwritten first-party helper: the dependency workflow must not add
// automatic instrumentation for workspace application code.
fn app_helper(value: u64) -> u64 { value.wrapping_add(3) }

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder().with_simple_exporter(exporter.clone()).build();
    opentelemetry::global::set_tracer_provider(provider.clone());
    let tracer = opentelemetry::global::tracer("app_root");

    // Two independent request roots whose dependency work interleaves.
    let root_a = opentelemetry::Context::current_with_span(tracer.start("request_a"));
    let root_b = opentelemetry::Context::current_with_span(tracer.start("request_b"));
    let trace_a = root_a.span().span_context().trace_id();
    let trace_b = root_b.span().span_context().trace_id();
    let root_a_id = root_a.span().span_context().span_id();
    let root_b_id = root_b.span().span_context().span_id();
    assert_ne!(trace_a, TraceId::INVALID);
    assert_ne!(trace_a, trace_b);
    assert_ne!(root_a_id, root_b_id);

    let (a_to_b, from_b) = tokio::sync::oneshot::channel::<()>();
    let (b_to_a, from_a) = tokio::sync::oneshot::channel::<()>();

    let branch_a = async move {
        // Synchronous nested chain under root A.
        assert_eq!(p31_dep::outer(10), 24);
        // First-party helper: called, but never instrumented in this workflow.
        assert_eq!(app_helper(1), 4);
        // Async chain with a deterministic yield suspension.
        assert_eq!(p31_dep::async_chain(5).await, 6);
        // Supported qualified tokio::spawn inside dependency source.
        assert_eq!(p31_dep::spawn_child().await, 22);
        // Unsupported bare spawn site: its dispatched task does not inherit
        // context, so the span it creates starts a separate trace.
        assert_eq!(p31_dep::unsupported_spawn().await, 31);
        // Interleave with root B: neither side may finish before both arrive.
        assert_eq!(p31_dep::rendezvous(a_to_b, from_a).await, 8);
        // Intentional exclusions exercised without generated spans.
        assert_eq!(p31_dep::inlined(3), 3 ^ 0xff);
        assert_eq!(p31_dep::fib(10), 55);
        // Lifecycle: creating and dropping a future without polling it
        // produces no body span.
        drop(p31_dep::never_polled());
        // Lifecycle: a started future that never completes is cancelled.
        let mut future = Box::pin(p31_dep::cancelled_work());
        let mut task_cx = core::task::Context::from_waker(core::task::Waker::noop());
        assert!(core::future::Future::poll(future.as_mut(), &mut task_cx).is_pending());
        // The suspended work must not leak its current context: once the poll
        // returns, the caller's root A context is restored.
        assert_eq!(
            opentelemetry::Context::current().span().span_context().span_id(),
            root_a_id,
            "caller context must be restored after polling suspends"
        );
        drop(future);
    };

    let branch_b = async move {
        // Independent work under root B, interleaved with root A.
        assert_eq!(p31_dep::outer(3), 10);
        assert_eq!(p31_dep::rendezvous(b_to_a, from_b).await, 8);
    };

    // Controlled interleaving through the dependency rendezvous; this fixture
    // exercises interleaving within one poll cycle, not thread migration.
    let _ = tokio::join!(
        branch_a.with_context(root_a.clone()),
        branch_b.with_context(root_b.clone())
    );

    // No context guard may remain attached after the branches complete.
    assert_eq!(
        opentelemetry::Context::current().span().span_context().span_id(),
        SpanId::INVALID,
        "context leaked after the request branches completed"
    );
    root_a.span().end();
    root_b.span().end();

    let spans = exporter.get_finished_spans().unwrap();

    // Application outputs were asserted above; assert span inventory next.
    let expected_counts: &[(&str, usize)] = &[
        ("request_a", 1),
        ("request_b", 1),
        ("outer", 2),
        ("mid", 2),
        ("leaf", 7), // 6 in-tree + 1 inside the unsupported bare-spawn task
        ("async_chain", 1),
        ("suspended", 2),
        ("spawn_child", 1),
        ("unsupported_spawn", 1),
        ("rendezvous", 2),
        ("cancelled_work", 1),
        ("never_polled", 0),
        ("inlined", 0),
        ("fib", 0),
        ("app_helper", 0),
    ];
    for (name, expected) in expected_counts {
        let found = spans.iter().filter(|s| s.name == *name).count();
        assert_eq!(found, *expected, "span count for {name}: {spans:?}");
    }
    assert_eq!(spans.len(), 21, "unexpected spans: {spans:?}");

    // Ownership: dependency spans come from the mirror scope; the manual roots
    // from the handwritten tracer; the application itself stays uninstrumented.
    for span in &spans {
        if span.name == "request_a" || span.name == "request_b" {
            assert_eq!(span.instrumentation_scope.name(), "app_root");
        } else {
            assert_eq!(
                span.instrumentation_scope.name(),
                "p31_dep",
                "span {} must come from the dependency instrumentation",
                span.name
            );
        }
    }
    assert!(!spans.iter().any(|s| s.instrumentation_scope.name() == "p31_app"));

    let find = |name: &str, parent: SpanId| {
        let matching: Vec<_> = spans
            .iter()
            .filter(|s| s.name == name && s.parent_span_id == parent)
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "expected exactly one {name} under its intended parent: {spans:?}"
        );
        matching[0]
    };

    // Root A ancestry: chain, async suspension, supported spawn, rendezvous,
    // and cancellation all parent under request_a.
    let outer_a = find("outer", root_a_id);
    let mid_a = find("mid", outer_a.span_context.span_id());
    find("leaf", mid_a.span_context.span_id());
    let async_chain_a = find("async_chain", root_a_id);
    let suspended_a = find("suspended", async_chain_a.span_context.span_id());
    find("leaf", suspended_a.span_context.span_id());
    let spawn_a = find("spawn_child", root_a_id);
    let suspended_spawn = find("suspended", spawn_a.span_context.span_id());
    find("leaf", suspended_spawn.span_context.span_id());
    find("unsupported_spawn", root_a_id);
    let rendezvous_a = find("rendezvous", root_a_id);
    find("leaf", rendezvous_a.span_context.span_id());
    find("cancelled_work", root_a_id);

    // Root B ancestry stays under request_b.
    let outer_b = find("outer", root_b_id);
    let mid_b = find("mid", outer_b.span_context.span_id());
    find("leaf", mid_b.span_context.span_id());
    let rendezvous_b = find("rendezvous", root_b_id);
    find("leaf", rendezvous_b.span_context.span_id());

    // Every span terminates at its own request root: independent roots never
    // acquire each other's descendants, and all descendants stay in their
    // intended trace. The unsupported bare-spawn task is the only sanctioned
    // trace escape and is classified explicitly below.
    let mut by_id = std::collections::HashMap::new();
    for span in &spans {
        by_id.insert(span.span_context.span_id(), span);
    }
    for span in &spans {
        if span.name == "request_a" || span.name == "request_b" {
            continue;
        }
        let expected_root = match span.span_context.trace_id() {
            trace if trace == trace_a => root_a_id,
            trace if trace == trace_b => root_b_id,
            _ => {
                assert_eq!(
                    span.parent_span_id,
                    SpanId::INVALID,
                    "only the unsupported bare-spawn task may escape both roots"
                );
                assert_eq!(span.name, "leaf");
                continue;
            }
        };
        let mut cursor = span;
        let mut steps = 0;
        while cursor.parent_span_id != SpanId::INVALID {
            cursor = *by_id
                .get(&cursor.parent_span_id)
                .unwrap_or_else(|| panic!("parent of {} missing from export", cursor.name));
            steps += 1;
            assert!(steps <= spans.len(), "parent chain cycle at {}", span.name);
        }
        assert_eq!(
            cursor.span_context.span_id(),
            expected_root,
            "span {} must terminate at its own request root",
            span.name
        );
    }

    // Unsupported spawn site classification: exactly one unparented non-root
    // span, in its own trace, never joined to either request.
    let orphans: Vec<_> = spans
        .iter()
        .filter(|s| {
            s.parent_span_id == SpanId::INVALID && s.name != "request_a" && s.name != "request_b"
        })
        .collect();
    assert_eq!(orphans.len(), 1, "unexpected unparented spans: {spans:?}");
    assert_eq!(orphans[0].name, "leaf");
    assert_ne!(orphans[0].span_context.trace_id(), trace_a);
    assert_ne!(orphans[0].span_context.trace_id(), trace_b);

    // Native dependency async lifecycle metadata (async spans only).
    let outcome = |name: &str, expected: &str, count: usize| {
        let found: Vec<_> = spans.iter().filter(|s| s.name == name).collect();
        assert_eq!(found.len(), count, "outcome count for {name}: {spans:?}");
        for span in found {
            assert!(
                span.attributes.iter().any(|a| {
                    a.key.as_str() == "cargo.instrumentation.async.outcome"
                        && a.value.as_str() == expected
                }),
                "outcome for {name}: {span:?}"
            );
        }
    };
    outcome("async_chain", "completed", 1);
    outcome("suspended", "completed", 2);
    outcome("spawn_child", "completed", 1);
    outcome("unsupported_spawn", "completed", 1);
    outcome("rendezvous", "completed", 2);
    outcome("cancelled_work", "cancelled", 1);

    println!("P31_TOTAL={}", spans.len());
    println!("P31_COMPLETE");
    provider.shutdown().unwrap();
}
"#;
