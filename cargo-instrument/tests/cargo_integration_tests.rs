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

/// Strip ANSI escape sequences from compiler/cargo output to ensure deterministic string matching.
fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if let Some(&'[') = chars.peek() {
                chars.next(); // consume '['
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if next.is_ascii_alphabetic() || next == '@' {
                        break;
                    }
                }
            } else if let Some(&'(') = chars.peek() {
                chars.next();
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
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
        .env("CARGO_TERM_COLOR", "never")
        .current_dir(ws_root)
        .output()
        .expect("clean build");
    let stdout1 = String::from_utf8_lossy(&output1.stdout);
    let stderr1 = strip_ansi(&String::from_utf8_lossy(&output1.stderr));
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
        .env("CARGO_TERM_COLOR", "never")
        .current_dir(ws_root)
        .output()
        .expect("repeat build");
    assert!(output2.status.success(), "Repeat build must succeed");
    let stderr2 = strip_ansi(&String::from_utf8_lossy(&output2.stderr));
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
        .env("CARGO_TERM_COLOR", "never")
        .current_dir(ws_root)
        .output()
        .expect("incremental app build");
    let stderr3 = strip_ansi(&String::from_utf8_lossy(&output3.stderr));
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
        "Incremental app build must NOT recompile unmodified dep_local. Stderr:\n{stderr3}"
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
        .env("CARGO_TERM_COLOR", "never")
        .current_dir(ws_root)
        .output()
        .expect("incremental dep build");
    assert!(
        output4.status.success(),
        "Incremental dep build must succeed"
    );
    let stderr4 = strip_ansi(&String::from_utf8_lossy(&output4.stderr));
    assert!(
        stderr4.contains("Compiling dep_local"),
        "Incremental build must recompile modified dep_local. Stderr:\n{stderr4}"
    );
    assert!(
        stderr4.contains("Compiling app"),
        "Modifying dependency must cause app to be recompiled/relinked. Stderr:\n{stderr4}"
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

#[test]
fn test_skipped_dependency_certification_and_repeat_build_freshness() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let ws_root = temp_dir.path();

    fs::write(
        ws_root.join("Cargo.toml"),
        r#"[workspace]
members = ["app", "dep_nostd", "dep_forbid", "dep_empty", "dep_trans"]
resolver = "2"
"#,
    )
    .expect("write root Cargo.toml");

    // 1. dep_nostd: #![no_std] crate
    let nostd_dir = ws_root.join("dep_nostd");
    fs::create_dir_all(nostd_dir.join("src")).expect("create dep_nostd src");
    fs::write(
        nostd_dir.join("Cargo.toml"),
        "[package]\nname = \"dep_nostd\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write dep_nostd Cargo.toml");
    fs::write(
        nostd_dir.join("src/lib.rs"),
        "#![no_std]\npub fn add(a: usize, b: usize) -> usize { a + b }\n",
    )
    .expect("write dep_nostd lib.rs");

    // 2. dep_forbid: #![forbid(unsafe_code)] crate
    let forbid_dir = ws_root.join("dep_forbid");
    fs::create_dir_all(forbid_dir.join("src")).expect("create dep_forbid src");
    fs::write(
        forbid_dir.join("Cargo.toml"),
        "[package]\nname = \"dep_forbid\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write dep_forbid Cargo.toml");
    fs::write(
        forbid_dir.join("src/lib.rs"),
        "#![forbid(unsafe_code)]\npub fn sub(a: usize, b: usize) -> usize { a.saturating_sub(b) }\n",
    )
    .expect("write dep_forbid lib.rs");

    // 3. dep_empty: zero candidate functions
    let empty_dir = ws_root.join("dep_empty");
    fs::create_dir_all(empty_dir.join("src")).expect("create dep_empty src");
    fs::write(
        empty_dir.join("Cargo.toml"),
        "[package]\nname = \"dep_empty\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write dep_empty Cargo.toml");
    fs::write(
        empty_dir.join("src/lib.rs"),
        "pub struct Marker;\npub const CONST_VAL: usize = 42;\n",
    )
    .expect("write dep_empty lib.rs");

    // 4. dep_trans: transformable crate
    let trans_dir = ws_root.join("dep_trans");
    fs::create_dir_all(trans_dir.join("src")).expect("create dep_trans src");
    fs::write(
        trans_dir.join("Cargo.toml"),
        "[package]\nname = \"dep_trans\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write dep_trans Cargo.toml");
    fs::write(
        trans_dir.join("src/lib.rs"),
        "pub fn mul(a: usize, b: usize) -> usize { a * b }\n",
    )
    .expect("write dep_trans lib.rs");

    let current_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = current_manifest
        .parent()
        .expect("cargo-instrument parent is repo root");
    let otel_shim_path = repo_root.join("otel-shim");
    let otel_shim_path_escaped = otel_shim_path.to_string_lossy().replace('\\', "/");

    // 5. app
    let app_dir = ws_root.join("app");
    fs::create_dir_all(app_dir.join("src")).expect("create app src");
    fs::write(
        app_dir.join("Cargo.toml"),
        format!(
            r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
dep_nostd = {{ path = "../dep_nostd" }}
dep_forbid = {{ path = "../dep_forbid" }}
dep_empty = {{ path = "../dep_empty" }}
dep_trans = {{ path = "../dep_trans" }}
otel-shim = {{ path = "{otel_shim_path_escaped}" }}
"#
        ),
    )
    .expect("write app Cargo.toml");
    fs::write(
        app_dir.join("src/main.rs"),
        r#"fn main() {
    otel_shim::init();
    let sum = dep_nostd::add(1, 2)
        + dep_forbid::sub(10, 4)
        + dep_empty::CONST_VAL
        + dep_trans::mul(3, 4);
    println!("sum: {sum}");
}
"#,
    )
    .expect("write app main.rs");

    // Generate initial Cargo.lock
    Command::new("cargo")
        .arg("generate-lockfile")
        .current_dir(ws_root)
        .status()
        .expect("generate lockfile");

    let cargo_instrument_bin = env!("CARGO_BIN_EXE_cargo-instrument");

    // Pass 1: Clean build
    let output1 = Command::new(cargo_instrument_bin)
        .args(["--", "build"])
        .env("CARGO_TERM_COLOR", "never")
        .current_dir(ws_root)
        .output()
        .expect("clean build");
    assert!(
        output1.status.success(),
        "Clean build must succeed! Stderr:\n{}",
        String::from_utf8_lossy(&output1.stderr)
    );

    // Verify stamp files in target/instrumented/debug/deps
    let deps_dir = ws_root.join("target/instrumented/debug/deps");
    let entries: Vec<String> = fs::read_dir(&deps_dir)
        .expect("read deps dir")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();

    // dep_nostd, dep_forbid, dep_empty must have skipped stamps
    assert!(
        entries
            .iter()
            .any(|e| e.starts_with(".cargo-instrument-skipped-dep_nostd")),
        "dep_nostd must have a skipped stamp. Entries: {entries:?}"
    );
    assert!(
        entries
            .iter()
            .any(|e| e.starts_with(".cargo-instrument-skipped-dep_forbid")),
        "dep_forbid must have a skipped stamp. Entries: {entries:?}"
    );
    assert!(
        entries
            .iter()
            .any(|e| e.starts_with(".cargo-instrument-skipped-dep_empty")),
        "dep_empty must have a skipped stamp. Entries: {entries:?}"
    );

    // dep_trans and app must have transformed stamps
    assert!(
        entries
            .iter()
            .any(|e| e.starts_with(".cargo-instrument-transformed-dep_trans")),
        "dep_trans must have a transformed stamp. Entries: {entries:?}"
    );
    assert!(
        entries
            .iter()
            .any(|e| e.starts_with(".cargo-instrument-transformed-app")),
        "app must have a transformed stamp. Entries: {entries:?}"
    );

    // Pass 2: Immediate repeat build with no changes
    let output2 = Command::new(cargo_instrument_bin)
        .args(["--", "build", "-vv"])
        .env("CARGO_TERM_COLOR", "never")
        .current_dir(ws_root)
        .output()
        .expect("repeat build");
    assert!(output2.status.success(), "Repeat build must succeed");
    let stderr2 = strip_ansi(&String::from_utf8_lossy(&output2.stderr));

    assert!(
        !stderr2.contains("Compiling dep_nostd"),
        "Repeat build must not recompile dep_nostd. Stderr:\n{stderr2}"
    );
    assert!(
        !stderr2.contains("Compiling dep_forbid"),
        "Repeat build must not recompile dep_forbid. Stderr:\n{stderr2}"
    );
    assert!(
        !stderr2.contains("Compiling dep_empty"),
        "Repeat build must not recompile dep_empty. Stderr:\n{stderr2}"
    );
    assert!(
        !stderr2.contains("Compiling dep_trans"),
        "Repeat build must not recompile dep_trans. Stderr:\n{stderr2}"
    );
    assert!(
        !stderr2.contains("Compiling app"),
        "Repeat build must not recompile app. Stderr:\n{stderr2}"
    );

    // Pass 3: Source change in a skipped crate triggers recompilation of only that crate and dependents
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::write(
        nostd_dir.join("src/lib.rs"),
        "#![no_std]\npub fn add(a: usize, b: usize) -> usize { a + b + 0 }\n",
    )
    .expect("touch dep_nostd lib.rs");

    let output3 = Command::new(cargo_instrument_bin)
        .args(["--", "build", "-vv"])
        .env("CARGO_TERM_COLOR", "never")
        .current_dir(ws_root)
        .output()
        .expect("rebuild after touch");
    assert!(output3.status.success(), "Rebuild after touch must succeed");
    let stderr3 = strip_ansi(&String::from_utf8_lossy(&output3.stderr));

    assert!(
        stderr3.contains("Compiling dep_nostd"),
        "Modified skipped crate must be recompiled. Stderr:\n{stderr3}"
    );
    assert!(
        stderr3.contains("Compiling app"),
        "Downstream app must be recompiled. Stderr:\n{stderr3}"
    );
    assert!(
        !stderr3.contains("Compiling dep_forbid"),
        "Unmodified dep_forbid must remain fresh. Stderr:\n{stderr3}"
    );
    assert!(
        !stderr3.contains("Compiling dep_empty"),
        "Unmodified dep_empty must remain fresh. Stderr:\n{stderr3}"
    );
    assert!(
        !stderr3.contains("Compiling dep_trans"),
        "Unmodified dep_trans must remain fresh. Stderr:\n{stderr3}"
    );

    // Pass 4: Subsequent repeat build is fresh again
    let output4 = Command::new(cargo_instrument_bin)
        .args(["--", "build", "-vv"])
        .env("CARGO_TERM_COLOR", "never")
        .current_dir(ws_root)
        .output()
        .expect("second repeat build");
    assert!(output4.status.success(), "Second repeat build must succeed");
    let stderr4 = strip_ansi(&String::from_utf8_lossy(&output4.stderr));
    assert!(
        !stderr4.contains("Compiling dep_nostd"),
        "Second repeat build must not recompile dep_nostd. Stderr:\n{stderr4}"
    );
    assert!(
        !stderr4.contains("Compiling app"),
        "Second repeat build must not recompile app. Stderr:\n{stderr4}"
    );
}

fn get_stamps_matching(deps_dir: &Path, prefix: &str) -> Vec<PathBuf> {
    if !deps_dir.exists() {
        return Vec::new();
    }
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(deps_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(prefix) {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    files
}

fn latest_mtime(paths: &[PathBuf]) -> Option<std::time::SystemTime> {
    paths
        .iter()
        .filter_map(|p| fs::metadata(p).and_then(|m| m.modified()).ok())
        .max()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedSpan {
    name: String,
    scope: String,
    trace_id: String,
    span_id: String,
    parent_span_id: String,
}

fn parse_spans(stdout: &str) -> (Option<usize>, Vec<ParsedSpan>) {
    let mut count = None;
    let mut spans = Vec::new();
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("SPANS_COUNT:") {
            count = rest.trim().parse().ok();
        } else if let Some(rest) = line.strip_prefix("SPAN:") {
            let mut name = String::new();
            let mut scope = String::new();
            let mut trace_id = String::new();
            let mut span_id = String::new();
            let mut parent_span_id = String::new();
            for part in rest.split_whitespace() {
                if let Some(val) = part
                    .strip_prefix("name='")
                    .and_then(|p| p.strip_suffix('\''))
                {
                    name = val.to_string();
                } else if let Some(val) = part
                    .strip_prefix("scope='")
                    .and_then(|p| p.strip_suffix('\''))
                {
                    scope = val.to_string();
                } else if let Some(val) = part
                    .strip_prefix("trace='")
                    .and_then(|p| p.strip_suffix('\''))
                {
                    trace_id = val.to_string();
                } else if let Some(val) =
                    part.strip_prefix("id='").and_then(|p| p.strip_suffix('\''))
                {
                    span_id = val.to_string();
                } else if let Some(val) = part
                    .strip_prefix("parent='")
                    .and_then(|p| p.strip_suffix('\''))
                {
                    parent_span_id = val.to_string();
                }
            }
            spans.push(ParsedSpan {
                name,
                scope,
                trace_id,
                span_id,
                parent_span_id,
            });
        }
    }
    (count, spans)
}

fn assert_trace_hierarchy(spans: &[ParsedSpan], expected_children: &[(&str, &str)]) {
    let root = spans
        .iter()
        .find(|s| s.name == "app_manual_root" && s.scope == "app")
        .expect("app_manual_root span must exist");
    assert_ne!(
        root.span_id, "0000000000000000",
        "root span must have valid non-zero id"
    );
    assert_ne!(
        root.trace_id, "00000000000000000000000000000000",
        "root span must have valid non-zero trace_id"
    );

    let mut matched_children = 0;
    for (exp_name, exp_scope) in expected_children {
        let child = spans
            .iter()
            .find(|s| s.name == *exp_name && s.scope == *exp_scope)
            .unwrap_or_else(|| panic!("child span {exp_name} with scope {exp_scope} must exist"));
        assert_eq!(
            child.trace_id, root.trace_id,
            "child span {exp_name} must share trace_id with root"
        );
        assert_eq!(
            child.parent_span_id, root.span_id,
            "child span {exp_name} must be parented to app_manual_root"
        );
        matched_children += 1;
    }

    for s in spans {
        if s.scope == "app" {
            assert_eq!(
                s.name, "app_manual_root",
                "workspace member 'app' must not have automatic instrumentation spans"
            );
        }
    }

    assert_eq!(
        spans.len(),
        matched_children + 1,
        "Total spans must match expected children plus 1 root"
    );
}

fn recompiled_units(stderr: &str) -> Vec<String> {
    let mut units = Vec::new();
    for line in stderr.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Compiling ") {
            let unit = rest.split_whitespace().next().unwrap_or_default();
            units.push(unit.to_string());
        }
    }
    units
}

struct InstrumentationFixture {
    _temp_dir: tempfile::TempDir,
    deps_dir: PathBuf,
    app_dir: PathBuf,
    target_dir: PathBuf,
    cargo_instrument_bin: PathBuf,
}

impl InstrumentationFixture {
    fn new() -> Self {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let root = temp_dir.path().to_path_buf();
        let deps_dir = root.join("deps");
        let app_dir = root.join("app");
        let target_dir = app_dir.join("target/instrumented");
        let cargo_instrument_bin = PathBuf::from(env!("CARGO_BIN_EXE_cargo-instrument"));

        let current_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo_root = current_manifest
            .parent()
            .expect("cargo-instrument parent is repo root");
        let otel_shim_path = repo_root.join("otel-shim");
        let otel_shim_escaped = otel_shim_path.to_string_lossy().replace('\\', "/");

        // 1. dep_trans
        let trans_dir = deps_dir.join("dep_trans");
        fs::create_dir_all(trans_dir.join("src")).expect("create dep_trans src");
        fs::write(
            trans_dir.join("Cargo.toml"),
            "[package]\nname = \"dep_trans\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n",
        )
        .expect("write dep_trans Cargo.toml");
        fs::write(
            trans_dir.join("src/lib.rs"),
            "pub fn compute(a: usize, b: usize) -> usize { a * b + 1 }\n",
        )
        .expect("write dep_trans lib.rs");

        // 2. dep_nostd
        let nostd_dir = deps_dir.join("dep_nostd");
        fs::create_dir_all(nostd_dir.join("src")).expect("create dep_nostd src");
        fs::write(
            nostd_dir.join("Cargo.toml"),
            "[package]\nname = \"dep_nostd\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n",
        )
        .expect("write dep_nostd Cargo.toml");
        fs::write(
            nostd_dir.join("src/lib.rs"),
            "#![no_std]\npub fn add(a: usize, b: usize) -> usize { a + b }\n",
        )
        .expect("write dep_nostd lib.rs");

        // 3. dep_forbid
        let forbid_dir = deps_dir.join("dep_forbid");
        fs::create_dir_all(forbid_dir.join("src")).expect("create dep_forbid src");
        fs::write(
            forbid_dir.join("Cargo.toml"),
            "[package]\nname = \"dep_forbid\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n",
        )
        .expect("write dep_forbid Cargo.toml");
        fs::write(
            forbid_dir.join("src/lib.rs"),
            "#![forbid(unsafe_code)]\npub fn sub(a: usize, b: usize) -> usize { a.saturating_sub(b) }\n",
        )
        .expect("write dep_forbid lib.rs");

        // 4. dep_empty
        let empty_dir = deps_dir.join("dep_empty");
        fs::create_dir_all(empty_dir.join("src")).expect("create dep_empty src");
        fs::write(
            empty_dir.join("Cargo.toml"),
            "[package]\nname = \"dep_empty\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n",
        )
        .expect("write dep_empty Cargo.toml");
        fs::write(
            empty_dir.join("src/lib.rs"),
            "pub struct Marker;\npub const CONST_VAL: usize = 42;\n",
        )
        .expect("write dep_empty lib.rs");

        // 5. app workspace
        fs::create_dir_all(app_dir.join("src")).expect("create app src");
        fs::write(
            app_dir.join("Cargo.toml"),
            format!(
                r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"

[workspace]

[dependencies]
dep_trans = {{ path = "../deps/dep_trans" }}
dep_nostd = {{ path = "../deps/dep_nostd" }}
dep_forbid = {{ path = "../deps/dep_forbid" }}
dep_empty = {{ path = "../deps/dep_empty" }}
otel-shim = {{ path = "{otel_shim_escaped}" }}
opentelemetry = "0.32.0"
opentelemetry_sdk = {{ version = "0.32.0", features = ["testing"] }}
"#
            ),
        )
        .expect("write app Cargo.toml");

        let fixture = Self {
            _temp_dir: temp_dir,
            deps_dir,
            app_dir,
            target_dir,
            cargo_instrument_bin,
        };

        fixture.write_app_main(false);

        Command::new("cargo")
            .arg("generate-lockfile")
            .current_dir(&fixture.app_dir)
            .status()
            .expect("generate lockfile");

        fixture
    }

    fn write_dep_source(&self, dep_name: &str, content: &str) {
        fs::write(self.deps_dir.join(dep_name).join("src/lib.rs"), content)
            .expect("write dep source");
    }

    fn write_app_main(&self, call_extra: bool) {
        let extra_call = if call_extra {
            "+ dep_empty::compute_extra(5)"
        } else {
            ""
        };
        fs::write(
            self.app_dir.join("src/main.rs"),
            format!(
                r#"use opentelemetry::trace::{{TraceContextExt, Tracer}};
use opentelemetry_sdk::trace::{{InMemorySpanExporter, SdkTracerProvider}};

fn main() {{
    otel_shim::init();
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    opentelemetry::global::set_tracer_provider(provider);

    let tracer = opentelemetry::global::tracer("app");
    let parent_span = tracer.start("app_manual_root");
    let cx = opentelemetry::Context::current_with_span(parent_span);
    let guard = cx.clone().attach();
    let base = dep_trans::compute(3, 4)
        + dep_nostd::add(1, 2)
        + dep_forbid::sub(10, 4)
        + dep_empty::CONST_VAL;
    let val = base {extra_call};
    drop(guard);
    cx.span().end();

    println!("APP_RESULT:{{val}}");

    let spans = exporter.get_finished_spans().unwrap();
    println!("SPANS_COUNT:{{}}", spans.len());
    for s in &spans {{
        println!(
            "SPAN:name='{{}}' scope='{{}}' trace='{{}}' id='{{}}' parent='{{}}'",
            s.name,
            s.instrumentation_scope.name(),
            s.span_context.trace_id(),
            s.span_context.span_id(),
            s.parent_span_id,
        );
    }}
}}
"#
            ),
        )
        .expect("write app main.rs");
    }

    fn run_instrumented(&self, extra_args: &[&str]) -> std::process::Output {
        Command::new(&self.cargo_instrument_bin)
            .args(["--with-dependencies", "--", "build"])
            .args(extra_args)
            .current_dir(&self.app_dir)
            .env("CARGO_TERM_COLOR", "never")
            .env_remove("CARGO_INSTRUMENT_DEPENDENCIES")
            .env_remove("CARGO_INSTRUMENT_REGISTRY")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("CARGO_INSTRUMENT_SESSION")
            .output()
            .expect("cargo-instrument execution")
    }

    fn run_ordinary_cargo(&self, extra_args: &[&str]) -> std::process::Output {
        Command::new("cargo")
            .args(["build", "--target-dir", "target/instrumented"])
            .args(extra_args)
            .current_dir(&self.app_dir)
            .env("CARGO_TERM_COLOR", "never")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("CARGO_INSTRUMENT_DEPENDENCIES")
            .env_remove("CARGO_INSTRUMENT_REGISTRY")
            .env_remove("CARGO_INSTRUMENT_SESSION")
            .output()
            .expect("ordinary cargo execution")
    }

    fn run_app(&self) -> std::process::Output {
        Command::new(self.target_dir.join("debug/app"))
            .output()
            .expect("run app")
    }

    fn read_policy(&self) -> Option<String> {
        fs::read_to_string(self.target_dir.join("cargo_instrument_policy")).ok()
    }

    fn get_stamps(&self, prefix: &str) -> Vec<PathBuf> {
        get_stamps_matching(&self.target_dir.join("debug/deps"), prefix)
    }

    fn latest_stamp_mtime(&self, prefix: &str) -> Option<std::time::SystemTime> {
        latest_mtime(&self.get_stamps(prefix))
    }
}

#[test]
fn test_scenario_d_instrumentable_path_dependency_automatic_source_change_invalidation() {
    let fixture = InstrumentationFixture::new();

    // 1. Initial clean build under public --with-dependencies policy
    let output1 = fixture.run_instrumented(&[]);
    assert!(
        output1.status.success(),
        "Initial build must succeed. Stderr:\n{}",
        String::from_utf8_lossy(&output1.stderr)
    );

    // 2. Validate policy recorded on disk is dependencies-v1
    assert_eq!(
        fixture.read_policy().as_deref(),
        Some("dependencies-v1"),
        "Build must record dependencies-v1 policy"
    );

    // 3. Verify workspace member (app) receives NO transformed or skipped stamps
    assert!(
        fixture
            .get_stamps(".cargo-instrument-transformed-app")
            .is_empty(),
        "Workspace member 'app' must not receive transformed stamps"
    );
    assert!(
        fixture
            .get_stamps(".cargo-instrument-skipped-app")
            .is_empty(),
        "Workspace member 'app' must not receive skipped stamps"
    );

    // 4. Verify external path dependencies receive correct stamps
    assert!(
        !fixture
            .get_stamps(".cargo-instrument-transformed-dep_trans")
            .is_empty(),
        "dep_trans must have transformed stamp"
    );
    let trans_mtime_1 = fixture
        .latest_stamp_mtime(".cargo-instrument-transformed-dep_trans")
        .expect("stamp mtime 1");

    assert!(
        !fixture
            .get_stamps(".cargo-instrument-skipped-dep_nostd")
            .is_empty(),
        "dep_nostd must have skipped stamp"
    );
    assert!(
        !fixture
            .get_stamps(".cargo-instrument-skipped-dep_forbid")
            .is_empty(),
        "dep_forbid must have skipped stamp"
    );
    assert!(
        !fixture
            .get_stamps(".cargo-instrument-skipped-dep_empty")
            .is_empty(),
        "dep_empty must have skipped stamp"
    );

    // 5. Execute app: verify output and structured trace hierarchy
    let run1 = fixture.run_app();
    assert!(run1.status.success());
    let stdout1 = String::from_utf8_lossy(&run1.stdout);
    assert!(
        stdout1.contains("APP_RESULT:64"),
        "Expected APP_RESULT:64 (13 + 3 + 6 + 42). Stdout:\n{stdout1}"
    );
    let (count1, spans1) = parse_spans(&stdout1);
    assert_eq!(count1, Some(2), "Expected 2 spans (root + compute)");
    assert_trace_hierarchy(&spans1, &[("compute", "dep_trans")]);

    // 6. Modify mutable path dependency source without touching manifest or cleaning
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fixture.write_dep_source(
        "dep_trans",
        "pub fn compute(a: usize, b: usize) -> usize { a * b + 100 }\n",
    );

    // 7. Rebuild with cargo-instrument
    let output2 = fixture.run_instrumented(&["-vv"]);
    assert!(
        output2.status.success(),
        "Rebuild must succeed. Stderr:\n{}",
        String::from_utf8_lossy(&output2.stderr)
    );
    let stderr2 = strip_ansi(&String::from_utf8_lossy(&output2.stderr));
    let recompiled2 = recompiled_units(&stderr2);

    assert!(
        recompiled2.contains(&"dep_trans".to_string()),
        "Modified dep_trans must recompile. Stderr:\n{stderr2}"
    );
    assert!(
        recompiled2.contains(&"app".to_string()),
        "Dependent app must recompile. Stderr:\n{stderr2}"
    );
    assert!(
        !recompiled2.contains(&"dep_nostd".to_string()),
        "dep_nostd must remain fresh. Stderr:\n{stderr2}"
    );
    assert!(
        !recompiled2.contains(&"dep_forbid".to_string()),
        "dep_forbid must remain fresh. Stderr:\n{stderr2}"
    );
    assert!(
        !recompiled2.contains(&"dep_empty".to_string()),
        "dep_empty must remain fresh. Stderr:\n{stderr2}"
    );

    // 8. Verify transformed stamp was updated
    let trans_mtime_2 = fixture
        .latest_stamp_mtime(".cargo-instrument-transformed-dep_trans")
        .expect("stamp mtime 2");
    assert!(
        trans_mtime_2 > trans_mtime_1,
        "Transformed stamp must be updated upon source modification"
    );

    // 9. Run updated application: verify new output and trace hierarchy
    let run2 = fixture.run_app();
    assert!(run2.status.success());
    let stdout2 = String::from_utf8_lossy(&run2.stdout);
    assert!(
        stdout2.contains("APP_RESULT:163"),
        "Expected APP_RESULT:163 (112 + 3 + 6 + 42). Stdout:\n{stdout2}"
    );
    let (count2, spans2) = parse_spans(&stdout2);
    assert_eq!(count2, Some(2), "Expected 2 spans after modification");
    assert_trace_hierarchy(&spans2, &[("compute", "dep_trans")]);

    // 10. Repeat build with no further changes: must be completely fresh
    let output3 = fixture.run_instrumented(&["-vv"]);
    assert!(output3.status.success());
    let stderr3 = strip_ansi(&String::from_utf8_lossy(&output3.stderr));
    let recompiled3 = recompiled_units(&stderr3);
    assert!(
        recompiled3.is_empty(),
        "Repeat build must not recompile any units. Stderr:\n{stderr3}"
    );
}

#[test]
fn test_scenario_e_skipped_path_dependency_source_modification_and_transition() {
    let fixture = InstrumentationFixture::new();

    // 1. Initial clean build under public --with-dependencies policy
    let output1 = fixture.run_instrumented(&[]);
    assert!(
        output1.status.success(),
        "Initial build must succeed. Stderr:\n{}",
        String::from_utf8_lossy(&output1.stderr)
    );
    assert_eq!(
        fixture.read_policy().as_deref(),
        Some("dependencies-v1"),
        "Build must record dependencies-v1 policy"
    );

    // Verify initial stamps
    assert!(!fixture
        .get_stamps(".cargo-instrument-skipped-dep_nostd")
        .is_empty());
    assert!(!fixture
        .get_stamps(".cargo-instrument-skipped-dep_forbid")
        .is_empty());
    assert!(!fixture
        .get_stamps(".cargo-instrument-skipped-dep_empty")
        .is_empty());
    assert!(fixture
        .get_stamps(".cargo-instrument-transformed-dep_nostd")
        .is_empty());
    assert!(fixture
        .get_stamps(".cargo-instrument-transformed-dep_forbid")
        .is_empty());
    assert!(fixture
        .get_stamps(".cargo-instrument-transformed-dep_empty")
        .is_empty());
    assert!(!fixture
        .get_stamps(".cargo-instrument-transformed-dep_trans")
        .is_empty());

    // 2. Part A: Source modification in skipped crate (dep_nostd) without manual cleaning
    let nostd_mtime_1 = fixture
        .latest_stamp_mtime(".cargo-instrument-skipped-dep_nostd")
        .expect("nostd stamp mtime 1");
    std::thread::sleep(std::time::Duration::from_millis(1100));

    fixture.write_dep_source(
        "dep_nostd",
        "#![no_std]\npub fn add(a: usize, b: usize) -> usize { a + b + 50 }\n",
    );

    let output_a = fixture.run_instrumented(&["-vv"]);
    assert!(output_a.status.success(), "Rebuild must succeed");
    let stderr_a = strip_ansi(&String::from_utf8_lossy(&output_a.stderr));
    let recompiled_a = recompiled_units(&stderr_a);

    assert!(
        recompiled_a.contains(&"dep_nostd".to_string()),
        "dep_nostd must recompile"
    );
    assert!(
        recompiled_a.contains(&"app".to_string()),
        "app must recompile"
    );
    assert!(
        !recompiled_a.contains(&"dep_forbid".to_string()),
        "dep_forbid must remain fresh"
    );
    assert!(
        !recompiled_a.contains(&"dep_empty".to_string()),
        "dep_empty must remain fresh"
    );
    assert!(
        !recompiled_a.contains(&"dep_trans".to_string()),
        "dep_trans must remain fresh"
    );

    let nostd_mtime_2 = fixture
        .latest_stamp_mtime(".cargo-instrument-skipped-dep_nostd")
        .expect("nostd stamp mtime 2");
    assert!(
        nostd_mtime_2 > nostd_mtime_1,
        "dep_nostd skipped stamp mtime must be refreshed"
    );
    assert!(
        fixture
            .get_stamps(".cargo-instrument-transformed-dep_nostd")
            .is_empty(),
        "dep_nostd must NOT have transformed stamp"
    );

    let run_a = fixture.run_app();
    assert!(run_a.status.success());
    let stdout_a = String::from_utf8_lossy(&run_a.stdout);
    assert!(
        stdout_a.contains("APP_RESULT:114"),
        "Expected 13 + 53 + 6 + 42 = 114. Stdout:\n{stdout_a}"
    );

    // Repeat build is fresh
    let repeat_a = fixture.run_instrumented(&["-vv"]);
    let recompiled_repeat_a =
        recompiled_units(&strip_ansi(&String::from_utf8_lossy(&repeat_a.stderr)));
    assert!(
        recompiled_repeat_a.is_empty(),
        "Repeat build a must be fresh"
    );

    // 3. Part B: Source modification in zero-candidate crate (dep_empty)
    let empty_mtime_1 = fixture
        .latest_stamp_mtime(".cargo-instrument-skipped-dep_empty")
        .expect("empty stamp mtime 1");
    std::thread::sleep(std::time::Duration::from_millis(1100));

    fixture.write_dep_source(
        "dep_empty",
        "pub struct Marker;\npub const CONST_VAL: usize = 99;\n",
    );

    let output_b = fixture.run_instrumented(&["-vv"]);
    assert!(output_b.status.success());
    let stderr_b = strip_ansi(&String::from_utf8_lossy(&output_b.stderr));
    let recompiled_b = recompiled_units(&stderr_b);

    assert!(
        recompiled_b.contains(&"dep_empty".to_string()),
        "dep_empty must recompile"
    );
    assert!(
        recompiled_b.contains(&"app".to_string()),
        "app must recompile"
    );
    assert!(
        !recompiled_b.contains(&"dep_nostd".to_string()),
        "dep_nostd must remain fresh"
    );
    assert!(
        !recompiled_b.contains(&"dep_forbid".to_string()),
        "dep_forbid must remain fresh"
    );
    assert!(
        !recompiled_b.contains(&"dep_trans".to_string()),
        "dep_trans must remain fresh"
    );

    let empty_mtime_2 = fixture
        .latest_stamp_mtime(".cargo-instrument-skipped-dep_empty")
        .expect("empty stamp mtime 2");
    assert!(
        empty_mtime_2 > empty_mtime_1,
        "dep_empty skipped stamp mtime must be refreshed"
    );
    assert!(
        fixture
            .get_stamps(".cargo-instrument-transformed-dep_empty")
            .is_empty(),
        "dep_empty must NOT have transformed stamp"
    );

    let run_b = fixture.run_app();
    assert!(run_b.status.success());
    let stdout_b = String::from_utf8_lossy(&run_b.stdout);
    assert!(
        stdout_b.contains("APP_RESULT:171"),
        "Expected 13 + 53 + 6 + 99 = 171. Stdout:\n{stdout_b}"
    );

    // 4. Part C: Transition from skipped (0 candidates) to instrumentable (candidate function added)
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fixture.write_dep_source(
        "dep_empty",
        "pub struct Marker;\npub const CONST_VAL: usize = 99;\npub fn compute_extra(x: usize) -> usize { x + 7 }\n",
    );
    fixture.write_app_main(true);

    let output_c = fixture.run_instrumented(&["-vv"]);
    assert!(
        output_c.status.success(),
        "Transition build must succeed. Stderr:\n{}",
        String::from_utf8_lossy(&output_c.stderr)
    );
    let stderr_c = strip_ansi(&String::from_utf8_lossy(&output_c.stderr));
    let recompiled_c = recompiled_units(&stderr_c);

    assert!(
        recompiled_c.contains(&"dep_empty".to_string()),
        "dep_empty must recompile on transition"
    );
    assert!(
        recompiled_c.contains(&"app".to_string()),
        "app must recompile on transition"
    );

    // Mutual exclusion: old skipped stamp must be REMOVED, and transformed stamp must be WRITTEN
    assert!(
        fixture
            .get_stamps(".cargo-instrument-skipped-dep_empty")
            .is_empty(),
        "dep_empty skipped stamp must be deleted when crate becomes instrumentable"
    );
    assert!(
        !fixture
            .get_stamps(".cargo-instrument-transformed-dep_empty")
            .is_empty(),
        "dep_empty transformed stamp must be written when crate becomes instrumentable"
    );

    let run_c = fixture.run_app();
    assert!(run_c.status.success());
    let stdout_c = String::from_utf8_lossy(&run_c.stdout);
    assert!(
        stdout_c.contains("APP_RESULT:183"),
        "Expected 171 + 12 = 183. Stdout:\n{stdout_c}"
    );
    let (count_c, spans_c) = parse_spans(&stdout_c);
    assert_eq!(
        count_c,
        Some(3),
        "Expected 3 spans (root + compute + compute_extra)"
    );
    assert_trace_hierarchy(
        &spans_c,
        &[("compute", "dep_trans"), ("compute_extra", "dep_empty")],
    );

    // Repeat build is fresh
    let repeat_c = fixture.run_instrumented(&["-vv"]);
    let recompiled_repeat_c =
        recompiled_units(&strip_ansi(&String::from_utf8_lossy(&repeat_c.stderr)));
    assert!(
        recompiled_repeat_c.is_empty(),
        "Repeat build c must be fresh"
    );

    // 5. Part D: Reverse transition: instrumentable becomes skipped (add #![no_std])
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fixture.write_dep_source(
        "dep_trans",
        "#![no_std]\npub fn compute(a: usize, b: usize) -> usize { a * b + 1 }\n",
    );

    let output_d = fixture.run_instrumented(&["-vv"]);
    assert!(output_d.status.success());
    let stderr_d = strip_ansi(&String::from_utf8_lossy(&output_d.stderr));
    let recompiled_d = recompiled_units(&stderr_d);

    assert!(
        recompiled_d.contains(&"dep_trans".to_string()),
        "dep_trans must recompile when becoming no_std"
    );
    assert!(
        recompiled_d.contains(&"app".to_string()),
        "app must recompile when dep_trans changes"
    );

    // Mutual exclusion: old transformed stamp must be REMOVED, and skipped stamp must be WRITTEN
    assert!(
        fixture
            .get_stamps(".cargo-instrument-transformed-dep_trans")
            .is_empty(),
        "dep_trans transformed stamp must be deleted when crate becomes no_std"
    );
    assert!(
        !fixture
            .get_stamps(".cargo-instrument-skipped-dep_trans")
            .is_empty(),
        "dep_trans skipped stamp must be written when crate becomes no_std"
    );

    let run_d = fixture.run_app();
    assert!(run_d.status.success());
    let stdout_d = String::from_utf8_lossy(&run_d.stdout);
    assert!(
        stdout_d.contains("APP_RESULT:183"),
        "Calculation remains 183. Stdout:\n{stdout_d}"
    );
    let (count_d, spans_d) = parse_spans(&stdout_d);
    assert_eq!(count_d, Some(2), "Expected 2 spans (root + compute_extra)");
    assert_trace_hierarchy(&spans_d, &[("compute_extra", "dep_empty")]);

    // Subsequent repeat build is fresh
    let repeat_d = fixture.run_instrumented(&["-vv"]);
    let recompiled_repeat_d =
        recompiled_units(&strip_ansi(&String::from_utf8_lossy(&repeat_d.stderr)));
    assert!(
        recompiled_repeat_d.is_empty(),
        "Repeat build d must be fresh"
    );
}

#[test]
fn test_scenario_h_plain_cargo_recompilation_and_reinstrumentation_transition() {
    let fixture = InstrumentationFixture::new();

    // 1. Initial instrumented build under --with-dependencies
    let output1 = fixture.run_instrumented(&[]);
    assert!(
        output1.status.success(),
        "Initial build must succeed. Stderr:\n{}",
        String::from_utf8_lossy(&output1.stderr)
    );
    assert_eq!(
        fixture.read_policy().as_deref(),
        Some("dependencies-v1"),
        "Build must record dependencies-v1 policy"
    );

    let run1 = fixture.run_app();
    assert!(run1.status.success());
    let stdout1 = String::from_utf8_lossy(&run1.stdout);
    assert!(
        stdout1.contains("APP_RESULT:64"),
        "Expected 64. Stdout:\n{stdout1}"
    );
    let (count1, spans1) = parse_spans(&stdout1);
    assert_eq!(count1, Some(2));
    assert_trace_hierarchy(&spans1, &[("compute", "dep_trans")]);

    let stamp_mtime_1 = fixture
        .latest_stamp_mtime(".cargo-instrument-transformed-dep_trans")
        .expect("stamp mtime");

    // 2. Plain Cargo reuse verification:
    // When no source changes, plain Cargo targeting target/instrumented reuses instrumented artifacts
    let output_plain_reuse = fixture.run_ordinary_cargo(&["-vv"]);
    assert!(output_plain_reuse.status.success());
    let stderr_reuse = strip_ansi(&String::from_utf8_lossy(&output_plain_reuse.stderr));
    let recompiled_reuse = recompiled_units(&stderr_reuse);
    assert!(
        recompiled_reuse.is_empty(),
        "Plain Cargo must reuse fresh instrumented artifacts without recompiling. Stderr:\n{stderr_reuse}"
    );

    // 3. Source modification under plain Cargo (without wrapper)
    std::thread::sleep(std::time::Duration::from_millis(1500));
    fixture.write_dep_source(
        "dep_trans",
        "pub fn compute(a: usize, b: usize) -> usize { a * b + 777 }\n",
    );

    let output_plain_rebuild = fixture.run_ordinary_cargo(&["-vv"]);
    assert!(output_plain_rebuild.status.success());
    let stderr_plain = strip_ansi(&String::from_utf8_lossy(&output_plain_rebuild.stderr));
    let recompiled_plain = recompiled_units(&stderr_plain);
    assert!(
        recompiled_plain.contains(&"dep_trans".to_string()),
        "Plain Cargo must recompile modified dep_trans"
    );
    assert!(
        recompiled_plain.contains(&"app".to_string()),
        "Plain Cargo must recompile dependent app"
    );

    // Verify: Plain Cargo updated the rlib artifact, but NOT the transformed stamp
    let trans_rlibs = fixture.get_stamps("libdep_trans");
    let rlib_mtime = latest_mtime(&trans_rlibs).expect("rlib mtime");
    let stamp_mtime_after_plain = fixture
        .latest_stamp_mtime(".cargo-instrument-transformed-dep_trans")
        .expect("stamp mtime");
    assert_eq!(
        stamp_mtime_1, stamp_mtime_after_plain,
        "Plain Cargo must not touch or refresh the instrumentation stamp"
    );
    assert!(
        rlib_mtime > stamp_mtime_after_plain + std::time::Duration::from_millis(500),
        "Plain Cargo produced an artifact newer than its certification stamp"
    );

    // Running the app compiled by plain Cargo reflects updated logic, but lacks dependency instrumentation
    let run_plain = fixture.run_app();
    assert!(run_plain.status.success());
    let stdout_plain = String::from_utf8_lossy(&run_plain.stdout);
    assert!(
        stdout_plain.contains("APP_RESULT:840"),
        "Expected 789 + 3 + 6 + 42 = 840. Stdout:\n{stdout_plain}"
    );
    let (count_plain, spans_plain) = parse_spans(&stdout_plain);
    assert_eq!(
        count_plain,
        Some(1),
        "Expected only root span under plain cargo"
    );
    assert_trace_hierarchy(&spans_plain, &[]);

    // 4. Run cargo-instrument again: tool identifies uncertified artifact (artifact.mtime > stamp.mtime)
    // and forces clean + re-instrumentation
    let output_reinstrument = fixture.run_instrumented(&["-vv"]);
    assert!(
        output_reinstrument.status.success(),
        "Recovery build must succeed. Stderr:\n{}",
        String::from_utf8_lossy(&output_reinstrument.stderr)
    );
    let stderr_reinst = strip_ansi(&String::from_utf8_lossy(&output_reinstrument.stderr));
    let recompiled_reinst = recompiled_units(&stderr_reinst);
    assert!(
        recompiled_reinst.contains(&"dep_trans".to_string()),
        "cargo-instrument must invalidate and recompile uncertified dep_trans. Stderr:\n{stderr_reinst}"
    );
    assert!(
        recompiled_reinst.contains(&"app".to_string()),
        "cargo-instrument must recompile app. Stderr:\n{stderr_reinst}"
    );

    // Verify: stamp was updated with fresh mtime >= artifact
    let stamp_mtime_reinst = fixture
        .latest_stamp_mtime(".cargo-instrument-transformed-dep_trans")
        .expect("stamp mtime reinst");
    assert!(
        stamp_mtime_reinst > stamp_mtime_after_plain,
        "Transformed stamp must be refreshed during re-instrumentation"
    );

    // Running app now restores expected spans!
    let run_reinst = fixture.run_app();
    assert!(run_reinst.status.success());
    let stdout_reinst = String::from_utf8_lossy(&run_reinst.stdout);
    assert!(
        stdout_reinst.contains("APP_RESULT:840"),
        "Output must remain 840. Stdout:\n{stdout_reinst}"
    );
    let (count_reinst, spans_reinst) = parse_spans(&stdout_reinst);
    assert_eq!(count_reinst, Some(2), "Restored 2 spans (root + compute)");
    assert_trace_hierarchy(&spans_reinst, &[("compute", "dep_trans")]);

    // 5. Subsequent repeat build is fresh
    let output_repeat = fixture.run_instrumented(&["-vv"]);
    assert!(output_repeat.status.success());
    let stderr_repeat = strip_ansi(&String::from_utf8_lossy(&output_repeat.stderr));
    let recompiled_repeat = recompiled_units(&stderr_repeat);
    assert!(
        recompiled_repeat.is_empty(),
        "Repeat build after reinstrumentation must be fresh"
    );
}
