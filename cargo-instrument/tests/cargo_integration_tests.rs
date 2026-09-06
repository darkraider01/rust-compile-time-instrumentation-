use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

/// Compute SHA-256 hash or byte contents of all source files in a directory.
fn snapshot_files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).expect("read_dir failed") {
        let entry = entry.expect("entry failed");
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .is_some_and(|ext| ext == "rs" || ext == "toml")
        {
            let bytes = fs::read(&path).expect("read file failed");
            files.push((path, bytes));
        } else if path.is_dir() {
            // Ignore cargo build artifact directory
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            files.extend(snapshot_files(&path));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

#[test]
fn test_cargo_wrapped_build_and_byte_preservation_invariant() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let fixture_root = temp_dir.path();

    // Create fixture Cargo.toml
    let cargo_toml = r#"
[package]
name = "fixture_crate"
version = "0.1.0"
edition = "2021"

[dependencies]
"#;
    fs::write(fixture_root.join("Cargo.toml"), cargo_toml).expect("write Cargo.toml");

    // Create fixture src/main.rs with diverse constructs and UTF-8 comments
    let src_dir = fixture_root.join("src");
    fs::create_dir_all(&src_dir).expect("create src dir");

    let main_rs = r#"// 🦀 UTF-8 Header comment with ferris and accent: é
pub fn compute() -> i32 {
    let mut val = 100;
    val += 42;
    val
}

pub async fn async_task() -> &'static str {
    "done"
}

struct Controller;

impl Controller {
    pub fn handle(&self) -> bool {
        true
    }
}

fn main() {
    println!("result: {}", compute());
}
"#;
    fs::write(src_dir.join("main.rs"), main_rs).expect("write main.rs");

    // 1. Snapshot all source files BEFORE running
    let before_snapshot = snapshot_files(fixture_root);
    assert!(
        !before_snapshot.is_empty(),
        "fixture should have source files"
    );

    // 2. Locate built cargo-instrument executable
    let cargo_instrument_bin = env!("CARGO_BIN_EXE_cargo-instrument");

    // 3. Run cargo check with RUSTC_WRAPPER set to cargo-instrument
    let output = Command::new("cargo")
        .arg("check")
        .current_dir(fixture_root)
        .env("RUSTC_WRAPPER", cargo_instrument_bin)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("failed to execute cargo check");

    assert!(output.status.success(), "wrapped cargo check must succeed");

    // M3: Assert candidate report and PID/crate tag appear on captured stderr
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[cargo-instrument PID="),
        "stderr should contain PID-tagged header. stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("crate=fixture_crate"),
        "stderr should identify fixture crate. stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("compute"),
        "stderr should contain candidate 'compute'. stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("async_task"),
        "stderr should contain candidate 'async_task'. stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("Controller::handle"),
        "stderr should contain candidate 'Controller::handle'. stderr:\n{stderr}"
    );

    // 4. Snapshot all source files AFTER running
    let after_snapshot = snapshot_files(fixture_root);

    // 5. Core Invariant Check: Source files MUST remain 100% byte-for-byte untouched
    assert_eq!(
        before_snapshot.len(),
        after_snapshot.len(),
        "Number of source files must remain identical"
    );

    for ((path_before, bytes_before), (path_after, bytes_after)) in
        before_snapshot.iter().zip(after_snapshot.iter())
    {
        assert_eq!(path_before, path_after, "File paths must match");
        assert_eq!(
            bytes_before, bytes_after,
            "File '{}' was modified! Core invariant violated: source must remain byte-for-byte untouched.",
            path_before.display()
        );
    }
}

#[test]
fn test_c1_cargo_wrapped_multi_file_discovery() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let fixture_root = temp_dir.path();

    let cargo_toml = r#"
[package]
name = "multifile_fixture"
version = "0.1.0"
edition = "2021"
"#;
    fs::write(fixture_root.join("Cargo.toml"), cargo_toml).expect("write Cargo.toml");

    let src_dir = fixture_root.join("src");
    fs::create_dir_all(&src_dir).expect("create src dir");

    let main_rs = r#"
mod helpers;

fn main() {
    helpers::submodule_work();
}
"#;
    let helpers_rs = r#"
pub fn submodule_work() -> i32 {
    100
}
"#;
    fs::write(src_dir.join("main.rs"), main_rs).expect("write main.rs");
    fs::write(src_dir.join("helpers.rs"), helpers_rs).expect("write helpers.rs");

    let cargo_instrument_bin = env!("CARGO_BIN_EXE_cargo-instrument");

    let output = Command::new("cargo")
        .arg("check")
        .current_dir(fixture_root)
        .env("RUSTC_WRAPPER", cargo_instrument_bin)
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("failed to execute cargo check");

    assert!(output.status.success(), "wrapped cargo check must succeed");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("submodule_work"),
        "stderr must contain function 'submodule_work' from submodule helpers.rs! stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("main"),
        "stderr must contain function 'main' from root main.rs! stderr:\n{stderr}"
    );
}

#[test]
fn test_h2_unisolated_target_dir_warning() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let fixture_root = temp_dir.path();

    let cargo_toml = r#"
[package]
name = "h2_warning_fixture"
version = "0.1.0"
edition = "2021"
"#;
    fs::write(fixture_root.join("Cargo.toml"), cargo_toml).expect("write Cargo.toml");

    let src_dir = fixture_root.join("src");
    fs::create_dir_all(&src_dir).expect("create src dir");
    fs::write(src_dir.join("main.rs"), "fn main() {}\n").expect("write main.rs");

    let cargo_instrument_bin = env!("CARGO_BIN_EXE_cargo-instrument");

    // Invoking cargo check directly with RUSTC_WRAPPER (without CARGO_INSTRUMENT_WRAPPER_MODE
    // and using default target/ directory) must trigger the ADR-004 unisolated warning.
    let output = Command::new("cargo")
        .arg("check")
        .current_dir(fixture_root)
        .env("RUSTC_WRAPPER", cargo_instrument_bin)
        .env_remove("CARGO_INSTRUMENT_WRAPPER_MODE")
        .output()
        .expect("failed to execute cargo check");

    assert!(output.status.success(), "cargo check must succeed");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("warning: cargo-instrument: compilation is not using an isolated target directory"),
        "stderr must contain ADR-004 unisolated target dir warning when wrapper mode is unset! stderr:\n{stderr}"
    );
}

#[test]
fn test_cli_isolated_target_dir_wiring() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let fixture_root = temp_dir.path();

    // Create minimal crate
    let cargo_toml = r#"
[package]
name = "target_dir_fixture"
version = "0.1.0"
edition = "2021"
"#;
    fs::write(fixture_root.join("Cargo.toml"), cargo_toml).expect("write Cargo.toml");

    let src_dir = fixture_root.join("src");
    fs::create_dir_all(&src_dir).expect("create src dir");
    fs::write(src_dir.join("main.rs"), "fn main() {}\n").expect("write main.rs");

    let cargo_instrument_bin = env!("CARGO_BIN_EXE_cargo-instrument");

    // Run `cargo-instrument -- check`
    let status = Command::new(cargo_instrument_bin)
        .arg("--")
        .arg("check")
        .current_dir(fixture_root)
        .status()
        .expect("failed to execute cargo-instrument");

    assert!(status.success(), "cargo-instrument CLI build must succeed");

    // Verify ADR-004: target/instrumented directory was created and contains build artifacts
    let instrumented_target = fixture_root.join("target").join("instrumented");
    assert!(
        instrumented_target.exists(),
        "ADR-004: target/instrumented must be created by default CLI wiring"
    );

    // Verify default target/ was NOT populated with debug artifacts (isolated cache)
    let default_target_debug = fixture_root.join("target").join("debug");
    assert!(
        !default_target_debug.exists(),
        "ADR-004: default target/debug must not be clobbered when running cargo-instrument"
    );
}

#[test]
fn test_cli_analyze_subcommand() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let test_file = temp_dir.path().join("sample.rs");
    fs::write(&test_file, "pub fn add(a: i32, b: i32) -> i32 { a + b }\n")
        .expect("write sample.rs");

    let cargo_instrument_bin = env!("CARGO_BIN_EXE_cargo-instrument");

    let output = Command::new(cargo_instrument_bin)
        .arg("analyze")
        .arg(&test_file)
        .output()
        .expect("failed to run cargo-instrument analyze");

    assert!(output.status.success(), "analyze command must exit 0");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("candidates:"));
    assert!(stdout.contains("add: bytes"));
}

fn compute_file_sha256(path: &Path) -> String {
    let bytes = fs::read(path).expect("read file for sha256");
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    format!("{:x}", hasher.finalize())
}

#[test]
fn test_cargo_correctness_clean_repeat_incremental_and_lock_immutability() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let ws_root = temp_dir.path();

    // 1. Workspace with app and local dependency
    let cargo_toml_content = r#"[workspace]
members = ["app", "dep_local"]
resolver = "2"
"#;
    fs::write(ws_root.join("Cargo.toml"), cargo_toml_content).expect("write root Cargo.toml");

    let dep_dir = ws_root.join("dep_local");
    let dep_src = dep_dir.join("src");
    fs::create_dir_all(&dep_src).expect("create dep_src");
    fs::write(
        dep_dir.join("Cargo.toml"),
        r#"[package]
name = "dep_local"
version = "0.1.0"
edition = "2021"
"#,
    )
    .expect("write dep Cargo.toml");
    let dep_lib = dep_src.join("lib.rs");
    fs::write(&dep_lib, "pub fn add(a: i32, b: i32) -> i32 { a + b }\n").expect("write dep lib.rs");

    let current_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = current_manifest
        .parent()
        .expect("cargo-instrument parent is repo root");
    let otel_shim_path = repo_root.join("otel-shim");
    let otel_shim_path_escaped = otel_shim_path.to_string_lossy().replace('\\', "/");

    let app_dir = ws_root.join("app");
    let app_src = app_dir.join("src");
    fs::create_dir_all(&app_src).expect("create app_src");
    fs::write(
        app_dir.join("Cargo.toml"),
        format!(
            r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
dep_local = {{ path = "../dep_local" }}
otel-shim = {{ path = "{otel_shim_path_escaped}" }}
"#
        ),
    )
    .expect("write app Cargo.toml");
    let app_main = app_src.join("main.rs");
    fs::write(
        &app_main,
        "fn main() { otel_shim::init(); println!(\"{}\", dep_local::add(1, 2)); }\n",
    )
    .expect("write app main.rs");

    // Generate initial Cargo.lock
    Command::new("cargo")
        .arg("generate-lockfile")
        .current_dir(ws_root)
        .status()
        .expect("generate lockfile");

    let initial_cargo_toml_hash = compute_file_sha256(&ws_root.join("Cargo.toml"));
    let initial_lock_hash = compute_file_sha256(&ws_root.join("Cargo.lock"));

    let cargo_instrument_bin = env!("CARGO_BIN_EXE_cargo-instrument");

    // Pass 1: Clean build
    let output1 = Command::new(cargo_instrument_bin)
        .args(["--", "build"])
        .current_dir(ws_root)
        .output()
        .expect("clean build");
    let stdout1 = String::from_utf8_lossy(&output1.stdout);
    let stderr1 = String::from_utf8_lossy(&output1.stderr);
    println!("CLEAN_BUILD_STDERR:\n{stderr1}");
    assert!(
        output1.status.success(),
        "Clean build must succeed!\nSTDOUT:\n{stdout1}\nSTDERR:\n{stderr1}"
    );

    // Target isolation check (A13): default target/debug must not exist, target/instrumented must exist
    assert!(
        ws_root.join("target").join("instrumented").exists(),
        "target/instrumented must exist"
    );
    assert!(
        !ws_root.join("target").join("debug").exists(),
        "default target/debug must not exist"
    );

    // Pass 2: Repeated build with no changes (A13: no rebuild)
    let output2 = Command::new(cargo_instrument_bin)
        .args(["--", "build", "-vv"])
        .current_dir(ws_root)
        .output()
        .expect("repeat build");
    assert!(output2.status.success(), "Repeat build must succeed");
    let stderr2 = String::from_utf8_lossy(&output2.stderr);
    println!("REPEAT_BUILD_STDERR:\n{stderr2}");
    assert!(
        !stderr2.contains("Compiling dep_local"),
        "Repeat build must not recompile dep_local. Stderr:\n{stderr2}"
    );
    assert!(
        !stderr2.contains("Compiling app"),
        "Repeat build must not recompile app. Stderr:\n{stderr2}"
    );

    // Pass 3: Incremental app touch (A13)
    std::thread::sleep(std::time::Duration::from_millis(100));
    fs::write(
        &app_main,
        "fn main() { otel_shim::init(); println!(\"result: {}\", dep_local::add(2, 3)); }\n",
    )
    .unwrap();
    let output3 = Command::new(cargo_instrument_bin)
        .args(["--", "build", "-vv"])
        .current_dir(ws_root)
        .output()
        .expect("incremental app build");
    let stderr3 = String::from_utf8_lossy(&output3.stderr);
    assert!(
        output3.status.success(),
        "Incremental app build must succeed! Stderr:\n{stderr3}"
    );
    assert!(
        stderr3.contains("Compiling app"),
        "Incremental build must recompile modified app. Stderr:\n{stderr3}"
    );
    assert!(
        !stderr3.contains("Compiling dep_local"),
        "Incremental app build must NOT recompile unmodified dep_local"
    );

    // Pass 4: Incremental dep touch (A13)
    std::thread::sleep(std::time::Duration::from_millis(100));
    fs::write(
        &dep_lib,
        "pub fn add(a: i32, b: i32) -> i32 { (a + b) * 1 }\n",
    )
    .unwrap();
    let output4 = Command::new(cargo_instrument_bin)
        .args(["--", "build"])
        .current_dir(ws_root)
        .output()
        .expect("incremental dep build");
    assert!(
        output4.status.success(),
        "Incremental dep build must succeed"
    );
    let stderr4 = String::from_utf8_lossy(&output4.stderr);
    assert!(
        stderr4.contains("Compiling dep_local"),
        "Incremental build must recompile modified dep_local"
    );
    assert!(
        stderr4.contains("Compiling app"),
        "Modifying dependency must cause app to be recompiled/relinked"
    );

    // Pass 5: Cargo graph immutability check (A14)
    let post_cargo_toml_hash = compute_file_sha256(&ws_root.join("Cargo.toml"));
    let post_lock_hash = compute_file_sha256(&ws_root.join("Cargo.lock"));
    assert_eq!(
        initial_cargo_toml_hash, post_cargo_toml_hash,
        "Cargo.toml hash must remain unchanged (A14)"
    );
    assert_eq!(
        initial_lock_hash, post_lock_hash,
        "Cargo.lock hash must remain unchanged (A14)"
    );
}

#[test]
fn test_cargo_error_propagation_and_coordinate_fidelity() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let fixture_root = temp_dir.path();

    let cargo_toml = r#"[package]
name = "error_fixture"
version = "0.1.0"
edition = "2021"
"#;
    fs::write(fixture_root.join("Cargo.toml"), cargo_toml).expect("write Cargo.toml");

    let src_dir = fixture_root.join("src");
    fs::create_dir_all(&src_dir).expect("create src dir");
    // Deliberate type error on line 2
    let bad_main =
        "fn main() {\n    let x: u32 = \"type mismatch error\";\n    println!(\"{x}\");\n}\n";
    fs::write(src_dir.join("main.rs"), bad_main).expect("write bad main.rs");

    let cargo_instrument_bin = env!("CARGO_BIN_EXE_cargo-instrument");
    let output = Command::new(cargo_instrument_bin)
        .args(["--", "check"])
        .current_dir(fixture_root)
        .output()
        .expect("run cargo check on erroneous source");

    // 1. Must exit non-zero
    assert!(
        !output.status.success(),
        "compilation with type error must exit non-zero"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    // 2. Error message must report type error
    assert!(
        stderr.contains("mismatched types") || stderr.contains("expected"),
        "stderr must contain rustc error description. Stderr:\n{stderr}"
    );
    // 3. Error coordinates must point to source file and snippet
    assert!(
        stderr.contains("main.rs") && stderr.contains("let x: u32 = \"type mismatch error\";"),
        "coordinates must point to the erroneous code snippet. Stderr:\n{stderr}"
    );
}
