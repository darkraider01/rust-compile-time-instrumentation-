//! H2 Feature-Safe Native OpenTelemetry Artifact Selection Tests.
//!
//! Validates:
//! 1. Valid native OpenTelemetry artifact selected when features are compatible (contains `trace`).
//! 2. Adversarial feature case: package, target, and profile match, but OpenTelemetry artifact
//!    lacks the required `trace` feature (e.g. only `metrics` enabled). Native selection safely drops
//!    native acquisition and fails open to Tier-2 C-ABI (`otel-shim`).
//! 3. Feature unification under Cargo resolver v2 across workspace roots.
//! 4. Target mismatch fail-open behavior.
//! 5. Profile mismatch fail-open behavior.
//! 6. Multiple OpenTelemetry versions / incompatible roots fail-open behavior.
//! 7. Source and manifest immutability across all runs.

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

fn write_shim(dir: &Path) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("Cargo.toml"),
        r#"[package]
name = "otel-shim"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();
    fs::write(
        dir.join("src/lib.rs"),
        r#"
#[no_mangle]
pub extern "C" fn __otel_span_enter(_name: *const u8, _len: usize) -> u64 { 1 }
#[no_mangle]
pub extern "C" fn __otel_span_exit(_id: u64) {}
pub fn init() {}
"#,
    )
    .unwrap();
}

// 1. Adversarial feature case:
// Application crate requires OpenTelemetry with `default-features = false, features = ["metrics"]` (lacks `trace`).
// Pre-pass compiles OpenTelemetry with only `metrics`.
// Native R-4 selection detects feature incompatibility (lacks mandatory `trace`),
// abandons native acquisition, invalidates dirty dependencies, and fails open to Tier-2 C-ABI.
// Application compiles cleanly and executes.
#[test]
fn test_h2_adversarial_feature_metrics_only_lacks_trace() {
    let temp = tempfile::tempdir().expect("create adversarial workspace");
    let workspace = temp.path();

    let shim = workspace.join("otel-shim");
    write_shim(&shim);

    fs::write(
        workspace.join("Cargo.toml"),
        r#"[workspace]
members = ["root_metrics", "dep_shared", "otel-shim"]
resolver = "2"
"#,
    )
    .unwrap();

    let dep = workspace.join("dep_shared");
    fs::create_dir_all(dep.join("src")).unwrap();
    fs::write(
        dep.join("Cargo.toml"),
        r#"[package]
name = "dep_shared"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();
    fs::write(
        dep.join("src/lib.rs"),
        r#"pub fn compute() -> u32 { 42 }
"#,
    )
    .unwrap();

    let root_metrics = workspace.join("root_metrics");
    fs::create_dir_all(root_metrics.join("src")).unwrap();
    fs::write(
        root_metrics.join("Cargo.toml"),
        r#"[package]
name = "root_metrics"
version = "0.1.0"
edition = "2021"

[dependencies]
dep_shared = { path = "../dep_shared" }
opentelemetry = { version = "0.32.0", default-features = false, features = ["metrics"] }
otel-shim = { path = "../otel-shim" }
"#,
    )
    .unwrap();
    fs::write(
        root_metrics.join("src/main.rs"),
        r#"fn main() {
    otel_shim::init();
    println!("METRICS_VAL={}", dep_shared::compute());
}
"#,
    )
    .unwrap();

    let before_snapshot = snapshot_tree(workspace);

    let output_metrics = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--package", "root_metrics", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("run root_metrics");

    assert_success(&output_metrics, "run root_metrics");
    let stdout_metrics = String::from_utf8_lossy(&output_metrics.stdout);
    assert!(
        stdout_metrics.contains("METRICS_VAL=42"),
        "expected successful execution of root_metrics via Tier-2 fallback:\n{stdout_metrics}"
    );
    let stderr_metrics = String::from_utf8_lossy(&output_metrics.stderr);
    assert!(
        stderr_metrics.contains("lacks required 'trace' feature"),
        "expected warning about missing trace feature:\n{stderr_metrics}"
    );
    assert!(
        stderr_metrics.contains("selecting Tier-2 C-ABI emitter"),
        "root_metrics must fall back to Tier-2 C-ABI emitter:\n{stderr_metrics}"
    );

    let after_snapshot = snapshot_tree(workspace);
    assert_eq!(
        before_snapshot, after_snapshot,
        "source and manifest files must remain byte-identical before and after build"
    );
}

// 2. Valid native artifact selected:
// Application crate requires OpenTelemetry with `trace` enabled (default features).
// Native R-4 selection validates feature compatibility and injects `--extern opentelemetry=...`.
// `dep_shared` selects `NativeOtelEmitter`.
#[test]
fn test_h2_valid_native_artifact_selected() {
    let temp = tempfile::tempdir().expect("create native workspace");
    let workspace = temp.path();

    let shim = workspace.join("otel-shim");
    write_shim(&shim);

    fs::write(
        workspace.join("Cargo.toml"),
        r#"[workspace]
members = ["root_native", "dep_shared", "otel-shim"]
resolver = "2"
"#,
    )
    .unwrap();

    let dep = workspace.join("dep_shared");
    fs::create_dir_all(dep.join("src")).unwrap();
    fs::write(
        dep.join("Cargo.toml"),
        r#"[package]
name = "dep_shared"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();
    fs::write(
        dep.join("src/lib.rs"),
        r#"pub fn compute() -> u32 { 99 }
"#,
    )
    .unwrap();

    let root_native = workspace.join("root_native");
    fs::create_dir_all(root_native.join("src")).unwrap();
    fs::write(
        root_native.join("Cargo.toml"),
        r#"[package]
name = "root_native"
version = "0.1.0"
edition = "2021"

[dependencies]
dep_shared = { path = "../dep_shared" }
opentelemetry = "0.32.0"
otel-shim = { path = "../otel-shim" }
"#,
    )
    .unwrap();
    fs::write(
        root_native.join("src/main.rs"),
        r#"fn main() {
    otel_shim::init();
    println!("NATIVE_VAL={}", dep_shared::compute());
}
"#,
    )
    .unwrap();

    let before_snapshot = snapshot_tree(workspace);

    let output_native = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--package", "root_native", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("run root_native");

    assert_success(&output_native, "run root_native");
    let stdout_native = String::from_utf8_lossy(&output_native.stdout);
    assert!(stdout_native.contains("NATIVE_VAL=99"));
    let stderr_native = String::from_utf8_lossy(&output_native.stderr);
    assert!(
        stderr_native.contains("crate=dep_shared] selecting native R-4 emitter"),
        "dep_shared must select native R-4 emitter:\n{stderr_native}"
    );
    assert!(
        stderr_native.contains("crate=dep_shared] injecting --extern opentelemetry="),
        "dep_shared must receive injected opentelemetry extern:\n{stderr_native}"
    );

    let after_snapshot = snapshot_tree(workspace);
    assert_eq!(
        before_snapshot, after_snapshot,
        "source and manifest files must remain byte-identical before and after build"
    );
}

// 3. Feature unification under Cargo resolver v2:
// Workspace contains `root_native` (requesting `trace`) and `root_metrics` (requesting `metrics`).
// When building the workspace, Cargo unifies features for target dependencies so `opentelemetry`
// is compiled with both `trace` and `metrics`.
// Native injection is valid and succeeds.
#[test]
fn test_h2_workspace_feature_unification_enables_native() {
    let temp = tempfile::tempdir().expect("create unified workspace");
    let workspace = temp.path();

    let shim = workspace.join("otel-shim");
    write_shim(&shim);

    fs::write(
        workspace.join("Cargo.toml"),
        r#"[workspace]
members = ["root_native", "root_metrics", "dep_shared", "otel-shim"]
resolver = "2"
"#,
    )
    .unwrap();

    let dep = workspace.join("dep_shared");
    fs::create_dir_all(dep.join("src")).unwrap();
    fs::write(
        dep.join("Cargo.toml"),
        r#"[package]
name = "dep_shared"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();
    fs::write(
        dep.join("src/lib.rs"),
        r#"pub fn compute() -> u32 { 100 }
"#,
    )
    .unwrap();

    let root_native = workspace.join("root_native");
    fs::create_dir_all(root_native.join("src")).unwrap();
    fs::write(
        root_native.join("Cargo.toml"),
        r#"[package]
name = "root_native"
version = "0.1.0"
edition = "2021"

[dependencies]
dep_shared = { path = "../dep_shared" }
opentelemetry = "0.32.0"
otel-shim = { path = "../otel-shim" }
"#,
    )
    .unwrap();
    fs::write(
        root_native.join("src/main.rs"),
        r#"fn main() {
    otel_shim::init();
    println!("NATIVE_VAL={}", dep_shared::compute());
}
"#,
    )
    .unwrap();

    let root_metrics = workspace.join("root_metrics");
    fs::create_dir_all(root_metrics.join("src")).unwrap();
    fs::write(
        root_metrics.join("Cargo.toml"),
        r#"[package]
name = "root_metrics"
version = "0.1.0"
edition = "2021"

[dependencies]
dep_shared = { path = "../dep_shared" }
opentelemetry = { version = "0.32.0", default-features = false, features = ["metrics"] }
otel-shim = { path = "../otel-shim" }
"#,
    )
    .unwrap();
    fs::write(
        root_metrics.join("src/main.rs"),
        r#"fn main() {
    otel_shim::init();
    println!("METRICS_VAL={}", dep_shared::compute());
}
"#,
    )
    .unwrap();

    let output_ws = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "build", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("build workspace");

    assert_success(&output_ws, "build workspace with unified features");
    let stderr = String::from_utf8_lossy(&output_ws.stderr);
    assert!(
        stderr.contains("crate=dep_shared] selecting native R-4 emitter"),
        "dep_shared must select native R-4 emitter under unified features:\n{stderr}"
    );
}

// 4. Target mismatch fail-open:
// Validates that when target mismatch occurs in `r4_native_otel_artifact_for`,
// it returns Err and safely falls open to Tier-2 without build failure.
#[test]
fn test_h2_target_mismatch_fails_open() {
    use cargo_instrument::SessionPlan;
    let temp = tempfile::tempdir().expect("create tempdir");
    let dep_dir = temp.path().join("dep");
    fs::create_dir_all(&dep_dir).unwrap();
    let source = dep_dir.join("src/lib.rs");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, "pub fn work() {}\n").unwrap();
    let rlib = temp.path().join("libopentelemetry-target.rlib");
    fs::write(&rlib, b"target artifact").unwrap();

    let dep_id = "dep 0.1.0 (path+file:///dep)";
    let otel_id = "registry+https://example.invalid#index#opentelemetry@0.32.0";
    let metadata = serde_json::json!({
        "packages": [
            {"id": dep_id, "name": "dep", "version": "0.1.0"},
            {"id": otel_id, "name": "opentelemetry", "version": "0.32.0"}
        ],
        "resolve": {"nodes": [
            {"id": otel_id, "features": ["trace"]}
        ]}
    });

    let mut plan = SessionPlan::default();
    plan.package_manifest_dirs
        .insert(dep_id.to_string(), dep_dir);
    plan.r4_otel_package_by_dependency
        .insert(dep_id.to_string(), otel_id.to_string());
    plan.r4_required_features_by_dependency
        .insert(dep_id.to_string(), vec!["trace".to_string()]);

    let message = serde_json::json!({
        "reason": "compiler-artifact",
        "package_id": otel_id,
        "filenames": [rlib],
        "features": ["trace"],
        "profile": {"opt_level": "0", "debug_assertions": true, "overflow_checks": true, "test": false}
    });

    // Artifact registered for target Some("x86_64-unknown-linux-gnu")
    plan.add_r4_artifacts_from_cargo_json(
        &metadata,
        format!("{message}\n").as_bytes(),
        Some("x86_64-unknown-linux-gnu".to_string()),
    )
    .expect("register target artifact");

    // Invocations with different target (e.g. None or wasm32) must fail
    let err = plan.r4_native_otel_artifact_for(&source, &[]).unwrap_err();
    assert!(err.contains("no Cargo-authoritative OpenTelemetry artifact"));

    let err2 = plan
        .r4_native_otel_artifact_for(
            &source,
            &["--target".to_string(), "wasm32-wasip1".to_string()],
        )
        .unwrap_err();
    assert!(err2.contains("no Cargo-authoritative OpenTelemetry artifact"));

    // Matching target succeeds
    let matched = plan
        .r4_native_otel_artifact_for(
            &source,
            &[
                "--target".to_string(),
                "x86_64-unknown-linux-gnu".to_string(),
            ],
        )
        .expect("matching target should succeed");
    assert!(matched.is_some());
}

// 5. Profile mismatch fail-open:
// Validates that profile mismatch in `r4_native_otel_artifact_for` returns Err and fails open.
#[test]
fn test_h2_profile_mismatch_fails_open() {
    use cargo_instrument::SessionPlan;
    let temp = tempfile::tempdir().expect("create tempdir");
    let dep_dir = temp.path().join("dep");
    fs::create_dir_all(&dep_dir).unwrap();
    let source = dep_dir.join("src/lib.rs");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, "pub fn work() {}\n").unwrap();
    let rlib = temp.path().join("libopentelemetry-profile.rlib");
    fs::write(&rlib, b"profile artifact").unwrap();

    let dep_id = "dep 0.1.0 (path+file:///dep)";
    let otel_id = "registry+https://example.invalid#index#opentelemetry@0.32.0";
    let metadata = serde_json::json!({
        "packages": [
            {"id": dep_id, "name": "dep", "version": "0.1.0"},
            {"id": otel_id, "name": "opentelemetry", "version": "0.32.0"}
        ],
        "resolve": {"nodes": [
            {"id": otel_id, "features": ["trace"]}
        ]}
    });

    let mut plan = SessionPlan::default();
    plan.package_manifest_dirs
        .insert(dep_id.to_string(), dep_dir);
    plan.r4_otel_package_by_dependency
        .insert(dep_id.to_string(), otel_id.to_string());
    plan.r4_required_features_by_dependency
        .insert(dep_id.to_string(), vec!["trace".to_string()]);

    let message = serde_json::json!({
        "reason": "compiler-artifact",
        "package_id": otel_id,
        "filenames": [rlib],
        "features": ["trace"],
        "profile": {"opt_level": "0", "debug_assertions": true, "overflow_checks": true, "test": false}
    });

    plan.add_r4_artifacts_from_cargo_json(&metadata, format!("{message}\n").as_bytes(), None)
        .expect("register dev artifact");

    // Release profile invocation (opt-level=3) encounters profile mismatch
    let err = plan
        .r4_native_otel_artifact_for(&source, &["-C".to_string(), "opt-level=3".to_string()])
        .unwrap_err();
    assert!(err.contains("no Cargo-authoritative OpenTelemetry artifact"));
}

// 6. Multiple OpenTelemetry versions / incompatible roots:
// Validates that when target roots disagree on OpenTelemetry version (e.g. 0.30 vs 0.32),
// the shared dependency is not mapped in `r4_otel_package_by_dependency` and falls open to Tier-2.
#[test]
fn test_h2_multiple_otel_versions_incompatible_roots() {
    use cargo_instrument::SessionPlan;
    let otel_30 = "registry+https://example.invalid#index#opentelemetry@0.30.0";
    let otel_32 = "registry+https://example.invalid#index#opentelemetry@0.32.0";
    let common = "registry+https://example.invalid#index#common@0.1.0";
    let app_a = "path+file:///app_a#0.1.0";
    let app_b = "path+file:///app_b#0.1.0";

    let metadata = serde_json::json!({
        "packages": [
            {"id": app_a, "name": "app-a", "version": "0.1.0", "targets": [{"kind": ["bin"]}]},
            {"id": app_b, "name": "app-b", "version": "0.1.0", "targets": [{"kind": ["bin"]}]},
            {"id": common, "name": "common", "version": "0.1.0", "targets": [{"kind": ["lib"]}]},
            {"id": otel_30, "name": "opentelemetry", "version": "0.30.0", "targets": [{"kind": ["lib"]}]},
            {"id": otel_32, "name": "opentelemetry", "version": "0.32.0", "targets": [{"kind": ["lib"]}]}
        ],
        "workspace_members": [app_a, app_b],
        "workspace_root": "/",
        "resolve": {"nodes": [
            {"id": app_a, "deps": [{"pkg": common}, {"pkg": otel_32}], "features": []},
            {"id": app_b, "deps": [{"pkg": common}, {"pkg": otel_30}], "features": []},
            {"id": common, "deps": [], "features": []},
            {"id": otel_30, "deps": [], "features": ["trace"]},
            {"id": otel_32, "deps": [], "features": ["trace"]}
        ]}
    });

    let plan = SessionPlan::from_metadata_json(&metadata).expect("parse multi-root metadata");
    assert!(
        !plan.r4_otel_package_by_dependency.contains_key(common),
        "a shared dependency reached by roots with 0.30 and 0.32 must be left on the fail-open path"
    );
    assert_eq!(
        plan.r4_otel_package_by_dependency.get(app_a),
        Some(&otel_32.to_string()),
        "single root retaining exact 0.32 identity"
    );
    assert_eq!(
        plan.r4_otel_package_by_dependency.get(app_b),
        Some(&otel_30.to_string()),
        "single root retaining exact 0.30 identity"
    );
}

// 7. Target-specific feature declarations:
// Validates that when target-specific dependencies declare different features (e.g. active OS has `metrics`,
// inactive OS has `trace`), the inactive declaration does NOT contaminate feature identity.
// The active compiled artifact lacks `trace`, so native application instrumentation MUST NOT be selected,
// and the build must succeed through Tier-2/S11 without `opentelemetry::trace` compile failures.
#[test]
fn test_h2_target_specific_features_do_not_contaminate_active_unit() {
    let temp = tempfile::tempdir().expect("create target-specific workspace");
    let workspace = temp.path();

    let shim = workspace.join("otel-shim");
    write_shim(&shim);

    fs::write(
        workspace.join("Cargo.toml"),
        r#"[workspace]
members = ["app_target_test", "dep_target_test", "otel-shim"]
resolver = "2"
"#,
    )
    .unwrap();

    let dep = workspace.join("dep_target_test");
    fs::create_dir_all(dep.join("src")).unwrap();
    fs::write(
        dep.join("Cargo.toml"),
        r#"[package]
name = "dep_target_test"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();
    fs::write(
        dep.join("src/lib.rs"),
        r#"pub fn run_dep() -> u32 { 77 }
"#,
    )
    .unwrap();

    let app = workspace.join("app_target_test");
    fs::create_dir_all(app.join("src")).unwrap();

    // Dynamically configure active OS with `metrics` (lacks `trace`),
    // and inactive OS with `trace`.
    // Under buggy manifest-scanning, the inactive `trace` declaration contaminates the build.
    // Under artifact-authoritative H2, only the active compiled artifact features count.
    let (active_cfg, inactive_cfg) = if cfg!(windows) {
        ("cfg(windows)", "cfg(unix)")
    } else {
        ("cfg(unix)", "cfg(windows)")
    };

    let app_manifest = format!(
        r#"[package]
name = "app_target_test"
version = "0.1.0"
edition = "2021"

[dependencies]
dep_target_test = {{ path = "../dep_target_test" }}
otel-shim = {{ path = "../otel-shim" }}

[target.'{active_cfg}'.dependencies]
opentelemetry = {{ version = "0.32.0", default-features = false, features = ["metrics"] }}

[target.'{inactive_cfg}'.dependencies]
opentelemetry = {{ version = "0.32.0", default-features = false, features = ["trace"] }}
"#
    );
    fs::write(app.join("Cargo.toml"), app_manifest).unwrap();
    fs::write(
        app.join("src/main.rs"),
        r#"fn main() {
    otel_shim::init();
    println!("TARGET_DEP_RES={}", dep_target_test::run_dep());
}
"#,
    )
    .unwrap();

    let before_snapshot = snapshot_tree(workspace);

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(["--", "run", "--package", "app_target_test", "--offline"])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("run app_target_test");

    assert_success(&output, "run app_target_test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("TARGET_DEP_RES=77"),
        "expected successful execution via Tier-2 fallback:\n{stdout}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("lacks required 'trace' feature"),
        "active artifact lacking trace must warn and reject native injection:\n{stderr}"
    );
    assert!(
        stderr.contains("selecting Tier-2 C-ABI emitter"),
        "must safely fall back to Tier-2 C-ABI emitter:\n{stderr}"
    );

    let after_snapshot = snapshot_tree(workspace);
    assert_eq!(
        before_snapshot, after_snapshot,
        "source and manifest files must remain byte-identical before and after build"
    );
}

// 8. Package name different from target name:
// Fixture: `[package] name = "my-app"`, `[[bin]] name = "server"`, `[dependencies] opentelemetry = "0.32.0"`.
// Validates:
// - Package identity resolves to `my-app` via `package_manifest_dirs`.
// - Rustc crate name is `server`.
// - Native application instrumentation is still selected when the active OTel artifact has `trace`.
#[test]
fn test_h2_package_name_differs_from_target_name() {
    let temp = tempfile::tempdir().expect("create package vs target workspace");
    let workspace = temp.path();

    fs::write(
        workspace.join("Cargo.toml"),
        r#"[workspace]
members = ["my_app"]
resolver = "2"
"#,
    )
    .unwrap();

    let app = workspace.join("my_app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("Cargo.toml"),
        r#"[package]
name = "my-app"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "server"
path = "src/main.rs"

[dependencies]
opentelemetry = "0.32.0"
"#,
    )
    .unwrap();
    fs::write(
        app.join("src/main.rs"),
        r#"fn work() -> u32 { 123 }

fn main() {
    println!("SERVER_OUTPUT={}", work());
}
"#,
    )
    .unwrap();

    let before_snapshot = snapshot_tree(workspace);

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args([
            "--",
            "run",
            "--package",
            "my-app",
            "--bin",
            "server",
            "--offline",
        ])
        .current_dir(workspace)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("run server binary");

    assert_success(&output, "run server binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("SERVER_OUTPUT=123"),
        "expected successful execution of server binary:\n{stdout}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("crate=server] selecting native OpenTelemetry emitter"),
        "target name 'server' from package 'my-app' must select native OpenTelemetry emitter:\n{stderr}"
    );

    let after_snapshot = snapshot_tree(workspace);
    assert_eq!(
        before_snapshot, after_snapshot,
        "source and manifest files must remain byte-identical before and after build"
    );
}

// 9. Missing artifact feature information unit test:
// Construct Cargo compiler-artifact JSON messages without features or with null features.
// Proves they are not accepted as feature-safe native evidence, and native resolution fails open.
#[test]
fn test_h2_missing_artifact_features_fails_open() {
    use cargo_instrument::SessionPlan;
    let temp = tempfile::tempdir().expect("create tempdir");
    let dep_dir = temp.path().join("dep");
    fs::create_dir_all(&dep_dir).unwrap();
    let source = dep_dir.join("src/lib.rs");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, "pub fn work() {}\n").unwrap();
    let rlib = temp.path().join("libopentelemetry-nofeats.rlib");
    fs::write(&rlib, b"nofeats artifact").unwrap();

    let dep_id = "dep 0.1.0 (path+file:///dep)";
    let otel_id = "registry+https://example.invalid#index#opentelemetry@0.32.0";
    let metadata = serde_json::json!({
        "packages": [
            {"id": dep_id, "name": "dep", "version": "0.1.0"},
            {"id": otel_id, "name": "opentelemetry", "version": "0.32.0"}
        ],
        "resolve": {"nodes": [
            {"id": otel_id, "features": ["trace"]}
        ]}
    });

    let mut plan = SessionPlan::default();
    plan.package_manifest_dirs
        .insert(dep_id.to_string(), dep_dir);
    plan.r4_otel_package_by_dependency
        .insert(dep_id.to_string(), otel_id.to_string());
    plan.r4_required_features_by_dependency
        .insert(dep_id.to_string(), vec!["trace".to_string()]);

    // Compiler-artifact message lacking "features" key
    let message_missing_features = serde_json::json!({
        "reason": "compiler-artifact",
        "package_id": otel_id,
        "filenames": [rlib],
        "profile": {"opt_level": "0", "debug_assertions": true, "overflow_checks": true, "test": false}
    });

    plan.add_r4_artifacts_from_cargo_json(
        &metadata,
        format!("{message_missing_features}\n").as_bytes(),
        None,
    )
    .expect("process artifact message");

    assert!(
        plan.r4_native_otel_artifacts.is_empty(),
        "artifact without 'features' key must not be accepted as native evidence"
    );

    let err = plan.r4_native_otel_artifact_for(&source, &[]).unwrap_err();
    assert!(
        err.contains("no Cargo-authoritative OpenTelemetry artifact"),
        "missing feature artifact must fail open: {err}"
    );

    // Compiler-artifact message with "features": null
    let message_null_features = serde_json::json!({
        "reason": "compiler-artifact",
        "package_id": otel_id,
        "filenames": [rlib],
        "features": null,
        "profile": {"opt_level": "0", "debug_assertions": true, "overflow_checks": true, "test": false}
    });

    plan.add_r4_artifacts_from_cargo_json(
        &metadata,
        format!("{message_null_features}\n").as_bytes(),
        None,
    )
    .expect("process artifact message with null features");

    assert!(
        plan.r4_native_otel_artifacts.is_empty(),
        "artifact with 'features': null must not be accepted as native evidence"
    );
}

// 10. Active rustc --extern path identity enforcement:
// Validates:
// - exact same path -> accepted;
// - lexically different but canonically identical path -> accepted;
// - same filename in two different directories -> rejected / fails open;
// - non-existent/stale extern path -> rejected / fails open;
// - existing valid H2 native path still succeeds.
#[test]
fn test_h2_extern_artifact_path_identity_enforcement() {
    use cargo_instrument::SessionPlan;
    let temp = tempfile::tempdir().expect("create tempdir");
    let dep_dir = temp.path().join("dep");
    fs::create_dir_all(&dep_dir).unwrap();
    let source = dep_dir.join("src/lib.rs");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, "pub fn work() {}\n").unwrap();

    let dir_a = temp.path().join("dir_a");
    let dir_b = temp.path().join("dir_b");
    fs::create_dir_all(&dir_a).unwrap();
    fs::create_dir_all(&dir_b).unwrap();

    let rlib_a = dir_a.join("libopentelemetry.rlib");
    fs::write(&rlib_a, b"artifact a").unwrap();
    let rlib_b = dir_b.join("libopentelemetry.rlib");
    fs::write(&rlib_b, b"artifact b").unwrap();

    let dep_id = "dep 0.1.0 (path+file:///dep)";
    let otel_id = "registry+https://example.invalid#index#opentelemetry@0.32.0";
    let metadata = serde_json::json!({
        "packages": [
            {"id": dep_id, "name": "dep", "version": "0.1.0"},
            {"id": otel_id, "name": "opentelemetry", "version": "0.32.0"}
        ],
        "resolve": {"nodes": [
            {"id": otel_id, "features": ["trace"]}
        ]}
    });

    let mut plan = SessionPlan::default();
    plan.package_manifest_dirs
        .insert(dep_id.to_string(), dep_dir);
    plan.r4_otel_package_by_dependency
        .insert(dep_id.to_string(), otel_id.to_string());
    plan.r4_required_features_by_dependency
        .insert(dep_id.to_string(), vec!["trace".to_string()]);

    let message = serde_json::json!({
        "reason": "compiler-artifact",
        "package_id": otel_id,
        "filenames": [rlib_a],
        "features": ["trace"],
        "profile": {"opt_level": "0", "debug_assertions": true, "overflow_checks": true, "test": false}
    });

    plan.add_r4_artifacts_from_cargo_json(&metadata, format!("{message}\n").as_bytes(), None)
        .expect("process artifact message");

    // Case 1: Exact same path -> accepted
    let args_exact = vec![
        "--extern".to_string(),
        format!("opentelemetry={}", rlib_a.display()),
    ];
    let artifact = plan
        .r4_native_otel_artifact_for(&source, &args_exact)
        .expect("exact path must resolve successfully")
        .expect("artifact must be found");
    assert_eq!(artifact.rlib_path, rlib_a);

    // Case 2: Lexically different but canonically identical path -> accepted
    let sub_a = dir_a.join("sub");
    fs::create_dir_all(&sub_a).unwrap();
    let lexical_same = sub_a.join("..").join("libopentelemetry.rlib");
    assert_ne!(rlib_a, lexical_same);
    let args_lexical = vec![
        "--extern".to_string(),
        format!("opentelemetry={}", lexical_same.display()),
    ];
    let artifact_lex = plan
        .r4_native_otel_artifact_for(&source, &args_lexical)
        .expect("lexically different but canonically identical path must resolve")
        .expect("artifact must be found");
    assert_eq!(artifact_lex.rlib_path, rlib_a);

    // Case 3: Same filename in two different directories -> rejected
    let args_different_dir = vec![
        "--extern".to_string(),
        format!("opentelemetry={}", rlib_b.display()),
    ];
    let err_diff = plan
        .r4_native_otel_artifact_for(&source, &args_different_dir)
        .unwrap_err();
    assert!(
        err_diff.contains("does not match active rustc --extern"),
        "same filename in different directory must be rejected: {err_diff}"
    );

    // Case 4: Non-existent/stale extern path -> rejected/fails open
    let stale_path = dir_a.join("stale_nonexistent.rlib");
    let args_stale = vec![
        "--extern".to_string(),
        format!("opentelemetry={}", stale_path.display()),
    ];
    let err_stale = plan
        .r4_native_otel_artifact_for(&source, &args_stale)
        .unwrap_err();
    assert!(
        err_stale.contains("does not match active rustc --extern"),
        "stale or non-existent extern path must be rejected: {err_stale}"
    );
}
