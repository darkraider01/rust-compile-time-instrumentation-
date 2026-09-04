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
    let status = Command::new("cargo")
        .arg("check")
        .current_dir(fixture_root)
        .env("RUSTC_WRAPPER", cargo_instrument_bin)
        .env("INSTRUMENT_DEBUG", "1")
        .status()
        .expect("failed to execute cargo check");

    assert!(status.success(), "wrapped cargo check must succeed");

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
