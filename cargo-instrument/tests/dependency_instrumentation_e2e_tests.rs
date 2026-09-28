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
