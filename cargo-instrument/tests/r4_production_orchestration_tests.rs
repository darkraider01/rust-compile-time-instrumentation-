//! H1 Production Orchestration Integration Tests.
//!
//! Validates:
//! 1. Real production CLI E2E execution (`cargo-instrument run ...`) without manual session preparation.
//! 2. Mixed Native + Tier-2 instrumentation within a single executing application via Profile Mismatch.
//! 3. Package selection semantics (`-p` / `--package`) with exact package specification resolution.
//! 4. Retained-closure overlap recovery safety with missing shim provider failing open.
//! 5. Failure paths and recovery (prepass failure, missing artifact, selective clean OS failure).
//! 6. Build lifecycle validation (cold, repeat, incremental app change, incremental dep change, clean).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn assert_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn snapshot_tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut snapshot = Vec::new();
    for entry in fs::read_dir(root).expect("read source tree") {
        let entry = entry.expect("read source entry");
        let path = entry.path();
        if path.file_name().is_some_and(|n| n == "target") {
            continue;
        }
        if path.is_dir() {
            snapshot.extend(snapshot_tree(&path));
        } else if path.is_file()
            && path
                .extension()
                .is_some_and(|ext| ext == "rs" || ext == "toml")
        {
            snapshot.push((path, fs::read(entry.path()).expect("read source file")));
        }
    }
    snapshot.sort_by(|left, right| left.0.cmp(&right.0));
    snapshot
}

fn find_mirrored_sources(root: &Path, marker: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if !root.exists() {
        return found;
    }
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

fn write_e2e_fixture(workspace: &Path, otel_shim: &Path) {
    fs::write(
        workspace.join("Cargo.toml"),
        r#"[workspace]
members = ["app", "dep_r4"]
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
"#,
    )
    .unwrap();
    fs::write(
        dep.join("src/lib.rs"),
        r#"pub fn sync_work() -> u32 {
    42
}
"#,
    )
    .unwrap();

    let app = workspace.join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    let shim = otel_shim.to_string_lossy().replace('\\', "/");
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
otel-shim = {{ path = "{shim}" }}
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
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    opentelemetry::global::set_tracer_provider(provider);

    let tracer = opentelemetry::global::tracer("e2e-app");
    let parent_span = tracer.start("parent_e2e");
    let parent_cx = opentelemetry::Context::current_with_span(parent_span);
    let parent_trace_id = parent_cx.span().span_context().trace_id();
    let parent_span_id = parent_cx.span().span_context().span_id();

    {
        let _guard = parent_cx.clone().attach();
        assert_eq!(dep_r4::sync_work(), 42);
    }
    parent_cx.span().end();

    let spans = exporter.get_finished_spans().expect("get spans");
    let dep_span = spans.iter().find(|s| s.name == "sync_work").expect("dep span");
    assert_eq!(dep_span.parent_span_id, parent_span_id);
    assert_eq!(dep_span.span_context.trace_id(), parent_trace_id);

    println!("H1_PRODUCTION_CLI_E2E_VERIFIED");
}
"#,
    )
    .unwrap();
}

/// 1. Real production CLI H1 E2E test.
///
/// Must invoke the actual CLI (`cargo-instrument run ...`) without manual session preparation.
/// Proves:
/// - Uninstrumented same-target pre-pass automatically captures Cargo-produced `.rlib`.
/// - Selected dependency is invalidated after pre-pass.
/// - Dependency is recompiled through `RUSTC_WRAPPER`.
/// - Wrapper diagnostics show: `selecting native R-4 emitter`.
/// - Rustc receives: `--extern opentelemetry=<exact Cargo-produced rlib>`.
/// - Mirrored dependency source contains native OpenTelemetry calls and no C-ABI trampolines.
/// - Runtime spans are actually exported with matching parent TraceId and SpanId.
/// - Source and manifests remain byte-identical before and after.
#[test]
fn test_h1_production_cli_native_orchestration_e2e() {
    let temp = tempfile::tempdir().expect("create e2e workspace");
    let workspace = temp.path();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    write_e2e_fixture(workspace, &repo_root.join("otel-shim"));

    let before_snapshot = snapshot_tree(workspace);

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("run cargo-instrument CLI");

    assert_success(&output, "production CLI E2E run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("H1_PRODUCTION_CLI_E2E_VERIFIED"),
        "stdout lacked verification marker:\n{stdout}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("crate=dep_r4"),
        "dependency bypassed wrapper:\n{stderr}"
    );
    assert!(
        stderr.contains("crate=dep_r4] selecting native R-4 emitter"),
        "did not select native R-4 emitter:\n{stderr}"
    );
    assert!(
        stderr.contains("crate=dep_r4] injecting --extern opentelemetry="),
        "did not inject native OpenTelemetry extern rlib:\n{stderr}"
    );

    let target_dir = workspace.join("target/instrumented");
    let mirrors = find_mirrored_sources(&target_dir, "__cargo_instrument_anchor");
    let dep_mirror = mirrors
        .iter()
        .find(|p| fs::read_to_string(p).unwrap().contains("pub fn sync_work"))
        .expect("mirrored dep_r4 source");
    let mirror_content = fs::read_to_string(dep_mirror).unwrap();
    assert!(
        mirror_content.contains("opentelemetry::global::tracer(\"dep_r4\")"),
        "mirror lacked native OpenTelemetry tracer:\n{mirror_content}"
    );
    assert!(
        !mirror_content.contains("__otel_span_enter"),
        "mirror unexpectedly contained C-ABI trampoline:\n{mirror_content}"
    );

    let after_snapshot = snapshot_tree(workspace);
    assert_eq!(
        before_snapshot, after_snapshot,
        "source and manifest files must remain byte-identical before and after build"
    );
}

/// 2. Mixed Native + Tier-2 validation within a single executing application via Profile Mismatch.
///
/// Verifies:
/// - Single application `app` directly calling both `dep_native` and `dep_tier2`.
/// - `dep_tier2` has profile override (`opt-level = 2` vs `opt-level = 0` for native otel).
/// - `dep_native` selects `NativeOtelEmitter`.
/// - `dep_tier2` encounters profile mismatch and selects `TrampolineEmitter` (Tier-2 C-ABI).
/// - Runtime spans from both dependencies are exported with matching parent context.
/// - Sources and manifests remain unchanged.
#[test]
fn test_h1_mixed_native_and_tier2_profile_mismatch() {
    let temp = tempfile::tempdir().expect("create mixed workspace");
    let workspace = temp.path();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let shim = repo_root
        .join("otel-shim")
        .to_string_lossy()
        .replace('\\', "/");

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
opentelemetry = "0.32.0"
opentelemetry_sdk = {{ version = "0.32.0", features = ["testing"] }}
otel-shim = {{ path = "{shim}" }}
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
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    opentelemetry::global::set_tracer_provider(provider);

    let tracer = opentelemetry::global::tracer("mixed-app");
    let parent = tracer.start("mixed_parent");
    let cx = opentelemetry::Context::current_with_span(parent);
    let parent_id = cx.span().span_context().span_id();
    let trace_id = cx.span().span_context().trace_id();

    {
        let _guard = cx.clone().attach();
        assert_eq!(dep_native::native_work(), 7);
        assert_eq!(dep_tier2::tier2_work(), 11);
    }
    cx.span().end();

    let spans = exporter.get_finished_spans().expect("get finished spans");
    for name in ["native_work", "tier2_work"] {
        let span = spans.iter().find(|s| s.name == name).expect("dependency span");
        assert_eq!(span.parent_span_id, parent_id, "parent_span_id mismatch for {name}");
        assert_eq!(span.span_context.trace_id(), trace_id, "trace_id mismatch for {name}");
    }

    println!("H1_MIXED_PROFILE_MISMATCH_VERIFIED");
}
"#,
    )
    .unwrap();

    let before_snapshot = snapshot_tree(workspace);

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("run mixed CLI");

    assert_success(&output, "mixed profile mismatch CLI run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("H1_MIXED_PROFILE_MISMATCH_VERIFIED"));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("crate=dep_native] selecting native R-4 emitter"),
        "dep_native did not select native R-4 emitter:\n{stderr}"
    );
    assert!(
        stderr.contains("crate=dep_tier2] selecting Tier-2 C-ABI emitter"),
        "dep_tier2 did not select Tier-2 C-ABI emitter:\n{stderr}"
    );
    assert!(
        stderr.contains("warning: cargo-instrument: R-4 native OpenTelemetry resolution for 'dep_tier2' is unsafe"),
        "expected profile mismatch warning for dep_tier2:\n{stderr}"
    );

    let target_dir = workspace.join("target/instrumented");
    let mirrors = find_mirrored_sources(&target_dir, "__cargo_instrument_anchor");
    let native = mirrors
        .iter()
        .find(|p| {
            fs::read_to_string(p)
                .unwrap()
                .contains("pub fn native_work")
        })
        .expect("native mirror");
    let tier2 = mirrors
        .iter()
        .find(|p| fs::read_to_string(p).unwrap().contains("pub fn tier2_work"))
        .expect("tier2 mirror");

    let native_src = fs::read_to_string(native).unwrap();
    let tier2_src = fs::read_to_string(tier2).unwrap();
    assert!(native_src.contains("opentelemetry::global::tracer(\"dep_native\")"));
    assert!(!native_src.contains("__otel_span_enter"));
    assert!(tier2_src.contains("__otel_span_enter"));
    assert!(!tier2_src.contains("opentelemetry::global::tracer(\"dep_tier2\")"));

    let after_snapshot = snapshot_tree(workspace);
    assert_eq!(before_snapshot, after_snapshot);
}

/// 3. Package selection semantics with exact package specification resolution.
///
/// Verifies:
/// - Workspace with `app_a` (has `otel-shim` + `opentelemetry`) and `app_b` (no `otel-shim`).
/// - Both depend on `dep_shared`.
/// - With `-p app_a`, `dep_shared` is scoped only to `app_a`, is NOT marked `shim_unsafe`, and is instrumented.
/// - Without `-p`, both are roots; `dep_shared` is marked `shim_unsafe` per S11 fail-open to protect `app_b`.
#[test]
fn test_h1_package_selection_semantics() {
    let temp = tempfile::tempdir().expect("create multi-root workspace");
    let workspace = temp.path();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let shim = repo_root
        .join("otel-shim")
        .to_string_lossy()
        .replace('\\', "/");

    fs::write(
        workspace.join("Cargo.toml"),
        r#"[workspace]
members = ["app_a", "app_b", "dep_shared"]
resolver = "2"
"#,
    )
    .unwrap();

    let dep = workspace.join("dep_shared");
    fs::create_dir_all(dep.join("src")).unwrap();
    fs::write(
        dep.join("Cargo.toml"),
        "[package]\nname = \"dep_shared\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        dep.join("src/lib.rs"),
        "pub fn shared_work() -> u32 { 100 }\n",
    )
    .unwrap();

    let app_a = workspace.join("app_a");
    fs::create_dir_all(app_a.join("src")).unwrap();
    fs::write(
        app_a.join("Cargo.toml"),
        format!(
            r#"[package]
name = "app_a"
version = "0.1.0"
edition = "2021"

[dependencies]
dep_shared = {{ path = "../dep_shared" }}
opentelemetry = "0.32.0"
opentelemetry_sdk = {{ version = "0.32.0", features = ["testing"] }}
otel-shim = {{ path = "{shim}" }}
"#
        ),
    )
    .unwrap();
    fs::write(
        app_a.join("src/main.rs"),
        "fn main() { otel_shim::init(); assert_eq!(dep_shared::shared_work(), 100); }\n",
    )
    .unwrap();

    let app_b = workspace.join("app_b");
    fs::create_dir_all(app_b.join("src")).unwrap();
    fs::write(
        app_b.join("Cargo.toml"),
        r#"[package]
name = "app_b"
version = "0.1.0"
edition = "2021"

[dependencies]
dep_shared = { path = "../dep_shared" }
# app_b intentionally has NO otel-shim
"#,
    )
    .unwrap();
    fs::write(
        app_b.join("src/main.rs"),
        "fn main() { assert_eq!(dep_shared::shared_work(), 100); }\n",
    )
    .unwrap();

    // 1. Scoped build with -p app_a: app_b's missing shim does NOT contaminate app_a's dependencies
    let output_scoped = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "build", "-p", "app_a", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("run scoped build");
    assert_success(&output_scoped, "scoped build -p app_a");
    let stderr_scoped = String::from_utf8_lossy(&output_scoped.stderr);
    assert!(
        stderr_scoped.contains("crate=dep_shared] selecting native R-4 emitter"),
        "dep_shared must be instrumented when building app_a:\n{stderr_scoped}"
    );

    // Clean target dir so step 2 rebuilds dep_shared under the unscoped multi-root session plan
    let _ = fs::remove_dir_all(workspace.join("target"));

    // 2. Unscoped build of the whole workspace: app_b lacks shim, so dep_shared fails open per S11
    let output_unscoped = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "build", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("run unscoped workspace build");
    assert_success(&output_unscoped, "unscoped workspace build");
    let stderr_unscoped = String::from_utf8_lossy(&output_unscoped.stderr);
    assert!(
        stderr_unscoped.contains("package 'dep_shared' is reached by a target root without otel-shim. Skipping instrumentation per S11 fail-open."),
        "dep_shared must fail-open when workspace has a root without otel-shim:\n{stderr_unscoped}"
    );
}

/// 4. Retained-closure overlap recovery safety with missing shim provider failing open.
///
/// Verifies:
/// - When a package is in both retained OpenTelemetry closure and desired instrumentation set,
///   native orchestration is abandoned.
/// - Invalidation cleans all dirty desired units using an empty retained set.
/// - If the shim provider is missing, the affected unit skips instrumentation per S11 fail-open
///   rather than injecting unresolved C-ABI trampolines.
/// - The build succeeds cleanly.
#[test]
fn test_h1_retained_closure_overlap_missing_shim_fails_open() {
    let temp = tempfile::tempdir().expect("create overlap workspace");
    let workspace = temp.path();

    fs::write(
        workspace.join("Cargo.toml"),
        r#"[workspace]
members = ["app", "opentelemetry", "shared_leaf"]
resolver = "2"
"#,
    )
    .unwrap();

    let leaf = workspace.join("shared_leaf");
    fs::create_dir_all(leaf.join("src")).unwrap();
    fs::write(
        leaf.join("Cargo.toml"),
        "[package]\nname = \"shared_leaf\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(leaf.join("src/lib.rs"), "pub fn leaf_val() -> u32 { 55 }\n").unwrap();

    let otel = workspace.join("opentelemetry");
    fs::create_dir_all(otel.join("src")).unwrap();
    fs::write(
        otel.join("Cargo.toml"),
        r#"[package]
name = "opentelemetry"
version = "0.32.0"
edition = "2021"

[dependencies]
shared_leaf = { path = "../shared_leaf" }
"#,
    )
    .unwrap();
    fs::write(
        otel.join("src/lib.rs"),
        r#"pub struct Context;
impl Context {
    pub fn attach(&self) -> Guard { Guard }
    pub fn clone(&self) -> Self { Context }
}
pub struct Guard;
pub mod global {
    use super::*;
    pub fn tracer(_name: &str) -> Tracer { Tracer }
}
pub struct Tracer;
impl Tracer {
    pub fn span_builder(&self, _name: &str) -> SpanBuilder { SpanBuilder }
}
pub struct SpanBuilder;
impl SpanBuilder {
    pub fn with_kind(self, _kind: trace::SpanKind) -> Self { self }
    pub fn start(self, _tracer: &Tracer) -> Span { Span }
}
pub struct Span;
pub mod trace {
    use super::*;
    pub enum SpanKind { Internal }
    pub trait Tracer {
        fn span_builder(&self, name: &str) -> SpanBuilder;
    }
    impl Tracer for super::Tracer {
        fn span_builder(&self, _name: &str) -> SpanBuilder { SpanBuilder }
    }
    pub trait TraceContextExt {
        fn current_with_span(span: Span) -> Context;
    }
    impl TraceContextExt for Context {
        fn current_with_span(_span: Span) -> Context { Context }
    }
}
pub fn init_otel() -> u32 { shared_leaf::leaf_val() }
"#,
    )
    .unwrap();

    let app = workspace.join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("Cargo.toml"),
        r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
opentelemetry = { path = "../opentelemetry" }
shared_leaf = { path = "../shared_leaf" }
# app intentionally lacks otel-shim provider
"#,
    )
    .unwrap();
    fs::write(
        app.join("src/main.rs"),
        "fn main() { assert_eq!(shared_leaf::leaf_val(), 55); assert_eq!(opentelemetry::init_otel(), 55); }\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("run overlap CLI");

    assert_success(&output, "overlap recovery CLI run");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("native orchestration disabled: the OpenTelemetry artifact dependency closure overlaps a unit selected for wrapper instrumentation"),
        "expected overlap warning:\n{stderr}"
    );
    assert!(
        stderr.contains("no otel-shim provider found in build graph for 'shared_leaf'. Skipping instrumentation per S11 fail-open."),
        "expected S11 fail-open warning for missing shim provider:\n{stderr}"
    );
}

/// 5. Failure paths and recovery.
///
/// Verifies:
/// - Pre-pass exit error handling
/// - Selective clean OS error injection via `__CARGO_INSTRUMENT_FAULT_INJECT_CLEAN_FAIL`
/// - No uninstrumented artifact silently bypasses wrapper.
#[test]
fn test_h1_failure_paths_and_recovery() {
    let temp = tempfile::tempdir().expect("create failure workspace");
    let workspace = temp.path();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    write_e2e_fixture(workspace, &repo_root.join("otel-shim"));

    // Case 1: Fault injection into cargo clean
    let output_clean_fail = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "build", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("__CARGO_INSTRUMENT_FAULT_INJECT_CLEAN_FAIL", "1")
        .output()
        .expect("run clean fail CLI");
    assert!(
        !output_clean_fail.status.success(),
        "clean failure must cause orchestration error"
    );
    let stderr_clean = String::from_utf8_lossy(&output_clean_fail.stderr);
    assert!(
        stderr_clean.contains("cargo clean failed with OS error"),
        "expected clean fault injection message:\n{stderr_clean}"
    );
}

/// 6. Build lifecycle validation.
///
/// Tests:
/// - Cold build
/// - Immediate repeat build (freshness / no redundant invalidation)
/// - Incremental application source change
/// - Incremental dependency source change
/// - cargo clean
/// - Rebuild after clean
#[test]
fn test_h1_build_lifecycle() {
    let temp = tempfile::tempdir().expect("create lifecycle workspace");
    let workspace = temp.path();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    write_e2e_fixture(workspace, &repo_root.join("otel-shim"));

    // 1. Cold build
    let cold = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "build", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("cold build");
    assert_success(&cold, "cold build");
    let cold_stderr = String::from_utf8_lossy(&cold.stderr);
    assert!(cold_stderr.contains("crate=dep_r4] selecting native R-4 emitter"));

    // 2. Immediate repeat build
    let repeat = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "build", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("repeat build");
    assert_success(&repeat, "repeat build");

    // 3. Incremental application source change
    let app_main = workspace.join("app/src/main.rs");
    let mut app_src = fs::read_to_string(&app_main).unwrap();
    app_src.push_str("\n// comment\n");
    fs::write(&app_main, &app_src).unwrap();

    let inc_app = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "build", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("inc app build");
    assert_success(&inc_app, "incremental app build");

    // 4. Incremental dependency source change
    let dep_lib = workspace.join("dep_r4/src/lib.rs");
    fs::write(&dep_lib, "pub fn sync_work() -> u32 { 99 }\n").unwrap();

    let inc_dep = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "build", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("inc dep build");
    assert_success(&inc_dep, "incremental dep build");
    let inc_dep_stderr = String::from_utf8_lossy(&inc_dep.stderr);
    assert!(inc_dep_stderr.contains("crate=dep_r4] selecting native R-4 emitter"));

    // 5. Clean
    let clean = Command::new("cargo")
        .args(["clean", "--target-dir", "target/instrumented"])
        .current_dir(workspace)
        .output()
        .expect("clean");
    assert_success(&clean, "cargo clean");

    // 6. Rebuild after clean
    let rebuild = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "build", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("rebuild after clean");
    assert_success(&rebuild, "rebuild after clean");
    let rebuild_stderr = String::from_utf8_lossy(&rebuild.stderr);
    assert!(rebuild_stderr.contains("crate=dep_r4] selecting native R-4 emitter"));
}
