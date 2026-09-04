use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
