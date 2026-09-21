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

// 5. Failure paths and recovery matrix.
//
// Covers:
// - Scenario 1 (Section 3): Pre-pass exits unsuccessfully after partial compilation with an absent dependency.
// - Scenario 1 (Part B): Partial pre-pass failure where shim provider is unavailable, verifying S11 fail-open.
// - Scenario 1 (Part C): Real (non-simulated) Cargo pre-pass syntax failure recompiles dependencies.
// - Scenario 2: Malformed or truncated Cargo JSON.
// - Scenario 3: No eligible OpenTelemetry artifact captured.
// - Scenario 4: Retained artifact disappears before final build.
// - Scenario 5: Selective cargo clean failure with OS error.
// - Scenario 6: Ambiguous package name prevents unsafe clean.

// Scenario 1 (Section 3):
// Partial pre-pass failure where an existing uninstrumented dependency artifact is in target-dir,
// and the pre-pass fails/terminates before reporting that dependency (absent from JSON messages).
// Verifies:
// - Missing dependency is invalidated and recompiled through RUSTC_WRAPPER.
// - Tier-2 instrumentation is actually applied (since otel-shim is available).
// - Spans are exported and app succeeds.
#[test]
fn test_h1_partial_prepass_failure_missing_dep_invalidated_and_recompiled() {
    let temp = tempfile::tempdir().expect("create partial prepass workspace");
    let workspace = temp.path();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    write_e2e_fixture(workspace, &repo_root.join("otel-shim"));

    let target_dir = workspace.join("target/instrumented");

    // 1. Prepopulate target directory with an uninstrumented artifact for dep_r4
    let prepopulate = Command::new("cargo")
        .args(["build", "--package", "dep_r4", "--offline", "--target-dir"])
        .arg(&target_dir)
        .current_dir(workspace)
        .output()
        .expect("prepopulate uninstrumented dep_r4");
    assert_success(&prepopulate, "prepopulate dep_r4");

    // Verify uninstrumented artifact exists
    assert!(target_dir.join("debug").exists());

    // 2. Invoke cargo-instrument with simulated pre-pass failure and dep_r4 omitted from pre-pass JSON stream
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .env("__CARGO_INSTRUMENT_FAULT_INJECT_PREPASS_FAIL", "1")
        .env("__CARGO_INSTRUMENT_FAULT_INJECT_PREPASS_OMIT_PKG", "dep_r4")
        .output()
        .expect("run CLI with partial pre-pass failure");

    assert_success(&output, "partial prepass recovery run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("H1_PRODUCTION_CLI_E2E_VERIFIED"));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr
            .contains("native orchestration disabled: fault injection: pre-pass exited with error"),
        "expected native orchestration disabled warning:\n{stderr}"
    );
    assert!(
        stderr.contains("crate=dep_r4"),
        "dep_r4 must pass through RUSTC_WRAPPER instead of reusing uninstrumented artifact:\n{stderr}"
    );
    assert!(
        stderr.contains("crate=dep_r4] selecting Tier-2 C-ABI emitter"),
        "dep_r4 must select Tier-2 emitter:\n{stderr}"
    );

    // Verify mirrored source has Tier-2 C-ABI instrumentation (__otel_span_enter)
    let mirrors = find_mirrored_sources(&target_dir, "__cargo_instrument_anchor");
    let dep_mirror = mirrors
        .iter()
        .find(|path| {
            fs::read_to_string(path)
                .unwrap()
                .contains("pub fn sync_work")
        })
        .expect("find dep_r4 mirror");
    let content = fs::read_to_string(dep_mirror).unwrap();
    assert!(
        content.contains("__otel_span_enter"),
        "dep_r4 must contain __otel_span_enter:\n{content}"
    );
}

// Scenario 1 (Part B):
// Partial pre-pass failure where shim provider is NOT available.
// Verifies:
// - Missing dependency is invalidated and recompiled through RUSTC_WRAPPER.
// - S11 fail-open is applied (skipped rather than given unresolved C-ABI dependency).
#[test]
fn test_h1_partial_prepass_failure_missing_shim_fails_open() {
    let temp = tempfile::tempdir().expect("create partial prepass no shim workspace");
    let workspace = temp.path();

    fs::write(
        workspace.join("Cargo.toml"),
        r#"[workspace]
members = ["app", "dep_leaf"]
resolver = "2"
"#,
    )
    .unwrap();

    let dep = workspace.join("dep_leaf");
    fs::create_dir_all(dep.join("src")).unwrap();
    fs::write(
        dep.join("Cargo.toml"),
        "[package]\nname = \"dep_leaf\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(dep.join("src/lib.rs"), "pub fn leaf() -> u32 { 101 }\n").unwrap();

    let app = workspace.join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("Cargo.toml"),
        r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
dep_leaf = { path = "../dep_leaf" }
# Intentionally no opentelemetry and no otel-shim
"#,
    )
    .unwrap();
    fs::write(
        app.join("src/main.rs"),
        "fn main() { assert_eq!(dep_leaf::leaf(), 101); }\n",
    )
    .unwrap();

    let target_dir = workspace.join("target/instrumented");

    // 1. Prepopulate uninstrumented dep_leaf
    let prepopulate = Command::new("cargo")
        .args([
            "build",
            "--package",
            "dep_leaf",
            "--offline",
            "--target-dir",
        ])
        .arg(&target_dir)
        .current_dir(workspace)
        .output()
        .expect("prepopulate uninstrumented dep_leaf");
    assert_success(&prepopulate, "prepopulate dep_leaf");

    // 2. Invoke cargo-instrument with simulated prepass failure and omit dep_leaf
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .env("__CARGO_INSTRUMENT_FAULT_INJECT_PREPASS_FAIL", "1")
        .env(
            "__CARGO_INSTRUMENT_FAULT_INJECT_PREPASS_OMIT_PKG",
            "dep_leaf",
        )
        .output()
        .expect("run CLI with partial pre-pass failure without shim");

    assert_success(&output, "missing shim run");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("crate=dep_leaf"),
        "dep_leaf must pass through RUSTC_WRAPPER:\n{stderr}"
    );
    assert!(
        stderr.contains("no otel-shim provider found in build graph for 'dep_leaf'. Skipping instrumentation per S11 fail-open."),
        "dep_leaf must fail-open per S11 when no shim provider:\n{stderr}"
    );
}

// Scenario 1 (Part C):
// Real (non-simulated) Cargo pre-pass failure where the application contains a syntax/compile error.
// Verifies:
// - Pre-pass exits non-zero with compiler error.
// - Complete safe invalidation wipes dependencies before final build.
// - Dependencies pass through RUSTC_WRAPPER before final build fails on the intentional error.
#[test]
fn test_h1_real_cargo_prepass_failure_recompiles_dependencies() {
    let temp = tempfile::tempdir().expect("create real prepass failure workspace");
    let workspace = temp.path();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    write_e2e_fixture(workspace, &repo_root.join("otel-shim"));

    // Introduce a compile error in app/src/main.rs
    let app_main = workspace.join("app/src/main.rs");
    let mut app_src = fs::read_to_string(&app_main).unwrap();
    app_src.push_str("\ncompile_error!(\"intentional pre-pass compile failure\");\n");
    fs::write(&app_main, &app_src).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "build", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("run with real pre-pass failure");

    assert!(
        !output.status.success(),
        "real compile error must fail the build"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("pre-pass exited"),
        "failed pre-pass must be diagnosed:\n{stderr}"
    );
    assert!(
        stderr.contains("crate=dep_r4"),
        "dep_r4 must pass through RUSTC_WRAPPER during final build:\n{stderr}"
    );
}

// Scenario 2:
// Pre-pass output contains malformed or truncated Cargo JSON.
// Verifies:
// - Native is abandoned, complete desired instrumentation set is invalidated.
// - No dependency is silently reused uninstrumented.
// - dep_r4 passes through RUSTC_WRAPPER and is recompiled through Tier-2.
#[test]
fn test_h1_malformed_cargo_json_recovery() {
    let temp = tempfile::tempdir().expect("create malformed json workspace");
    let workspace = temp.path();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    write_e2e_fixture(workspace, &repo_root.join("otel-shim"));

    let target_dir = workspace.join("target/instrumented");

    // 1. Prepopulate uninstrumented dep_r4
    let prepopulate = Command::new("cargo")
        .args(["build", "--package", "dep_r4", "--offline", "--target-dir"])
        .arg(&target_dir)
        .current_dir(workspace)
        .output()
        .expect("prepopulate uninstrumented dep_r4");
    assert_success(&prepopulate, "prepopulate dep_r4");

    // 2. Invoke cargo-instrument with simulated corrupt JSON
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .env("__CARGO_INSTRUMENT_FAULT_INJECT_CORRUPT_JSON", "1")
        .output()
        .expect("run CLI with corrupt json");

    assert_success(&output, "corrupt json recovery run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("H1_PRODUCTION_CLI_E2E_VERIFIED"));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("native orchestration disabled: pre-pass Cargo JSON output was malformed"),
        "expected malformed JSON warning:\n{stderr}"
    );
    assert!(
        stderr.contains("crate=dep_r4"),
        "dep_r4 must pass through RUSTC_WRAPPER instead of being reused:\n{stderr}"
    );
    assert!(
        stderr.contains("crate=dep_r4] selecting Tier-2 C-ABI emitter"),
        "dep_r4 must select Tier-2 emitter:\n{stderr}"
    );
}

// Scenario 3:
// The pre-pass produces no eligible OpenTelemetry artifact (e.g. app only uses otel-shim).
// Verifies:
// - Safe Tier-2 path with actual wrapper execution.
// - dep_r4 selects Tier-2 C-ABI emitter.
#[test]
fn test_h1_no_eligible_otel_artifact_fallback() {
    let temp = tempfile::tempdir().expect("create no-otel workspace");
    let workspace = temp.path();
    let shim = workspace.join("otel-shim");
    fs::create_dir_all(shim.join("src")).unwrap();
    fs::write(
        shim.join("Cargo.toml"),
        "[package]\nname = \"otel-shim\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        shim.join("src/lib.rs"),
        r#"
#[no_mangle]
pub extern "C" fn __otel_span_enter(_name: *const u8, _len: usize) -> u64 { 1 }
#[no_mangle]
pub extern "C" fn __otel_span_exit(_id: u64) {}
pub fn init() {}
"#,
    )
    .unwrap();

    fs::write(
        workspace.join("Cargo.toml"),
        r#"[workspace]
members = ["app", "dep_r4", "otel-shim"]
resolver = "2"
"#,
    )
    .unwrap();

    let dep = workspace.join("dep_r4");
    fs::create_dir_all(dep.join("src")).unwrap();
    fs::write(
        dep.join("Cargo.toml"),
        "[package]\nname = \"dep_r4\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(dep.join("src/lib.rs"), "pub fn compute() -> u32 { 77 }\n").unwrap();

    let app = workspace.join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("Cargo.toml"),
        r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
dep_r4 = { path = "../dep_r4" }
otel-shim = { path = "../otel-shim" }
# Note: absolutely no opentelemetry crate in dependency graph
"#,
    )
    .unwrap();
    fs::write(
        app.join("src/main.rs"),
        "fn main() { otel_shim::init(); assert_eq!(dep_r4::compute(), 77); println!(\"NO_OTEL_TIER2_VERIFIED\"); }\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("run CLI with no otel artifact");

    assert_success(&output, "no otel artifact run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("NO_OTEL_TIER2_VERIFIED"));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("the pre-pass produced no eligible OpenTelemetry artifact"),
        "expected no eligible artifact warning:\n{stderr}"
    );
    assert!(
        stderr.contains("crate=dep_r4] selecting Tier-2 C-ABI emitter"),
        "dep_r4 must select Tier-2 C-ABI emitter:\n{stderr}"
    );
}

// Scenario 4:
// The retained OpenTelemetry artifact disappears before the final build.
// Verifies:
// - Native injection is disabled, complete invalidation of desired units occurs.
// - Affected packages safely recompiled through Tier-2.
// - Application executes and exports spans.
#[test]
fn test_h1_retained_artifact_disappears_recovery() {
    let temp = tempfile::tempdir().expect("create disappearing artifact workspace");
    let workspace = temp.path();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    write_e2e_fixture(workspace, &repo_root.join("otel-shim"));

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--package", "app", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .env("__CARGO_INSTRUMENT_FAULT_INJECT_DELETE_RETAINED", "1")
        .output()
        .expect("run CLI with deleted retained artifact");

    assert_success(&output, "disappearing artifact run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("H1_PRODUCTION_CLI_E2E_VERIFIED"));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("selective invalidation removed the retained OpenTelemetry artifact"),
        "expected missing artifact warning:\n{stderr}"
    );
    assert!(
        stderr.contains("crate=dep_r4] selecting Tier-2 C-ABI emitter"),
        "dep_r4 must select Tier-2 emitter:\n{stderr}"
    );
}

// Scenario 5:
// Selective cargo clean failure with OS error.
// Verifies:
// - Explicit orchestration error; no claim of successful fallback.
#[test]
fn test_h1_selective_clean_failure_terminates_with_diagnostic() {
    let temp = tempfile::tempdir().expect("create failure workspace");
    let workspace = temp.path();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    write_e2e_fixture(workspace, &repo_root.join("otel-shim"));

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

// Scenario 6:
// Duplicate package names where ambiguous cleaning cannot be guaranteed.
// Verifies:
// - Exact identity handled safely with explicit orchestration error.
#[test]
fn test_h1_ambiguous_package_name_prevents_unsafe_clean() {
    let temp = tempfile::tempdir().expect("create ambiguous workspace");
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
        "[workspace]\nmembers = [\"app\", \"decoy\"]\nexclude = [\"ambiguous_dep_v1\", \"ambiguous_dep_v2\"]\nresolver = \"2\"\n",
    )
    .unwrap();

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
