use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use cargo_instrument::{
    is_valid_prepass_token, CargoDepEdge, R4NativeOtelArtifact, R4Profile, SessionPlan,
    PREPASS_FINGERPRINT_ENV, PREPASS_SESSION_ENV, PREPASS_TOKEN_ENV, WRAPPER_MODE_ENV,
};

static MOCK_RUSTC_DIR: OnceLock<tempfile::TempDir> = OnceLock::new();

fn get_mock_rustc() -> PathBuf {
    let temp = MOCK_RUSTC_DIR.get_or_init(|| {
        let dir = tempfile::tempdir().expect("create tempdir for mock rustc");
        let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
        let src = dir.path().join("mock_rustc.rs");
        let exe_name = if cfg!(windows) {
            "mock_rustc.exe"
        } else {
            "mock_rustc"
        };
        let out = dir.path().join(exe_name);
        let code = r#"
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Ok(log_path) = std::env::var("MOCK_RUSTC_LOG") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_path) {
            for arg in &args {
                let _ = writeln!(f, "{}", arg);
            }
        }
    }
    let code: i32 = std::env::var("MOCK_RUSTC_EXIT_CODE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    std::process::exit(code);
}
"#;
        std::fs::write(&src, code).expect("write mock rustc source");
        let status = Command::new(&rustc)
            .arg(&src)
            .arg("-o")
            .arg(&out)
            .status()
            .expect("compile mock rustc");
        assert!(status.success(), "mock rustc must compile");
        dir
    });
    let exe_name = if cfg!(windows) {
        "mock_rustc.exe"
    } else {
        "mock_rustc"
    };
    temp.path().join(exe_name)
}

fn read_mock_args(log_path: &Path) -> Vec<String> {
    if !log_path.exists() {
        return Vec::new();
    }
    fs::read_to_string(log_path)
        .expect("read mock log")
        .lines()
        .map(String::from)
        .collect()
}

fn setup_test_crate(dir: &Path) -> (PathBuf, PathBuf) {
    let src_dir = dir.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let source_file = src_dir.join("lib.rs");
    fs::write(&source_file, b"pub fn original_function() -> i32 { 42 }\n").unwrap();
    let out_dir = dir.join("out");
    fs::create_dir_all(&out_dir).unwrap();
    (source_file, out_dir)
}

fn setup_session_with_otel(dir: &Path, crate_source: &Path) -> (SessionPlan, PathBuf, PathBuf) {
    let otel_dir = dir.join("otel_search_path");
    fs::create_dir_all(&otel_dir).unwrap();
    let rlib_path = otel_dir.join("libopentelemetry.rlib");
    fs::write(&rlib_path, b"dummy rlib content").unwrap();

    let crate_id = "test_crate 0.1.0".to_string();
    let otel_id = "opentelemetry 0.27.0".to_string();

    let mut plan = SessionPlan {
        fingerprint: "valid_fingerprint_hash_1234".to_string(),
        ..Default::default()
    };
    plan.package_names_by_id
        .insert(crate_id.clone(), "test_crate".into());
    plan.package_names_by_id
        .insert(otel_id.clone(), "opentelemetry".into());
    plan.package_manifest_dirs
        .insert(crate_id.clone(), dir.to_path_buf());
    plan.r4_otel_package_by_dependency
        .insert(crate_id.clone(), otel_id.clone());

    plan.package_dependencies.insert(
        crate_id.clone(),
        vec![CargoDepEdge {
            binding_name: "opentelemetry".into(),
            package_id: otel_id.clone(),
            kinds: vec![None],
        }],
    );
    plan.package_dependencies.insert(otel_id.clone(), vec![]);

    let profile = R4Profile {
        opt_level: "3".to_string(),
        debug_assertions: false,
        overflow_checks: false,
        test: false,
    };

    plan.r4_native_otel_artifacts.push(R4NativeOtelArtifact {
        package_id: otel_id,
        package_version: "0.27.0".to_string(),
        resolved_features: vec!["trace".into()],
        target: None,
        profile,
        rlib_path,
        search_paths: vec![otel_dir.clone()],
    });

    let _ = crate_source;
    let session_file = dir.join("cargo_instrument_session.json");
    plan.save_to_file(&session_file).unwrap();

    (plan, session_file, otel_dir)
}

#[test]
fn test_prepass_wrapper_token_format_validation() {
    assert!(is_valid_prepass_token("prepass_sess123_456"));
    assert!(is_valid_prepass_token("prepass_sess123_456_789"));
    assert!(is_valid_prepass_token("prepass_sess-123_456_789_abcdef"));

    assert!(!is_valid_prepass_token(""));
    assert!(!is_valid_prepass_token("prepass"));
    assert!(!is_valid_prepass_token("prepass_"));
    assert!(!is_valid_prepass_token("prepass__456"));
    assert!(!is_valid_prepass_token("prepass_123_"));
    assert!(!is_valid_prepass_token("prepass_123")); // only 1 part after prefix
    assert!(!is_valid_prepass_token("token_without_prefix"));
    assert!(!is_valid_prepass_token("prepass_123/456"));
    assert!(!is_valid_prepass_token("prepass_123;456"));
}

#[test]
fn test_prepass_wrapper_valid_session_injects_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (source_file, out_dir) = setup_test_crate(temp.path());
    let (plan, session_file, otel_dir) = setup_session_with_otel(temp.path(), &source_file);
    let mock_rustc = get_mock_rustc();
    let log_path = temp.path().join("mock_rustc.log");

    let source_before = fs::read_to_string(&source_file).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .arg(&mock_rustc)
        .args([
            "--crate-name",
            "test_crate",
            source_file.to_str().unwrap(),
            "--crate-type",
            "lib",
            "--out-dir",
            out_dir.to_str().unwrap(),
            "-C",
            "opt-level=3",
        ])
        .env(WRAPPER_MODE_ENV, "1")
        .env(PREPASS_TOKEN_ENV, "prepass_sess123_456_789")
        .env(PREPASS_SESSION_ENV, &session_file)
        .env(PREPASS_FINGERPRINT_ENV, &plan.fingerprint)
        .env("MOCK_RUSTC_LOG", &log_path)
        .output()
        .expect("invoke cargo-instrument in pre-pass wrapper mode");

    assert!(
        output.status.success(),
        "pre-pass wrapper should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let recorded_args = read_mock_args(&log_path);
    assert!(!recorded_args.is_empty(), "mock rustc must be executed");

    // Search path injected: -L and dependency=<otel_dir>
    let expected_flag = format!("dependency={}", otel_dir.display());
    let has_search_path = recorded_args
        .windows(2)
        .any(|w| w[0] == "-L" && w[1] == expected_flag);
    assert!(
        has_search_path,
        "mock rustc arguments must contain injected -L dependency={}",
        otel_dir.display()
    );

    // Invariant: source file is unmodified (no transformation)
    let source_after = fs::read_to_string(&source_file).unwrap();
    assert_eq!(
        source_before, source_after,
        "pre-pass mode must never transform source code"
    );

    // Invariant: no stamp files created in out_dir
    let stamps: Vec<_> = fs::read_dir(&out_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .contains(".cargo-instrument-")
        })
        .collect();
    assert!(
        stamps.is_empty(),
        "pre-pass wrapper must not write stamp files"
    );
}

#[test]
fn test_prepass_wrapper_missing_session_runs_unmodified_without_fallthrough() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (source_file, out_dir) = setup_test_crate(temp.path());
    let mock_rustc = get_mock_rustc();
    let log_path = temp.path().join("mock_rustc.log");

    let source_before = fs::read_to_string(&source_file).unwrap();

    // Invocation with PREPASS_TOKEN_ENV present, but PREPASS_SESSION_ENV absent.
    // Pre-pass mode must remain active, run mock rustc without search paths,
    // and NEVER fall through to normal wrapper.
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .arg(&mock_rustc)
        .args([
            "--crate-name",
            "test_crate",
            source_file.to_str().unwrap(),
            "--crate-type",
            "lib",
            "--out-dir",
            out_dir.to_str().unwrap(),
            "-C",
            "opt-level=3",
        ])
        .env(WRAPPER_MODE_ENV, "1")
        .env(PREPASS_TOKEN_ENV, "prepass_sess123_456_789")
        .env_remove(PREPASS_SESSION_ENV)
        .env("MOCK_RUSTC_LOG", &log_path)
        .output()
        .expect("invoke cargo-instrument with missing session");

    assert!(
        output.status.success(),
        "missing session in pre-pass should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let recorded_args = read_mock_args(&log_path);
    assert!(!recorded_args.is_empty(), "mock rustc must be executed");

    // Invariant: no -L dependency flags injected
    assert!(
        !recorded_args
            .iter()
            .any(|arg| arg.starts_with("dependency=")),
        "missing session must not inject any dependency search paths"
    );

    // Invariant: source file is unmodified (no transformation)
    let source_after = fs::read_to_string(&source_file).unwrap();
    assert_eq!(
        source_before, source_after,
        "pre-pass must not transform source code"
    );

    // Invariant: no stamp files created
    let stamps: Vec<_> = fs::read_dir(&out_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .contains(".cargo-instrument-")
        })
        .collect();
    assert!(
        stamps.is_empty(),
        "pre-pass wrapper must not write stamp files"
    );
}

#[test]
fn test_prepass_wrapper_corrupted_session_runs_safely() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (source_file, out_dir) = setup_test_crate(temp.path());
    let mock_rustc = get_mock_rustc();
    let log_path = temp.path().join("mock_rustc.log");

    // Write malformed JSON to session file
    let corrupt_session = temp.path().join("corrupted_session.json");
    fs::write(&corrupt_session, b"{\n  not-valid-json: true\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .arg(&mock_rustc)
        .args([
            "--crate-name",
            "test_crate",
            source_file.to_str().unwrap(),
            "--crate-type",
            "lib",
            "--out-dir",
            out_dir.to_str().unwrap(),
            "-C",
            "opt-level=3",
        ])
        .env(WRAPPER_MODE_ENV, "1")
        .env(PREPASS_TOKEN_ENV, "prepass_sess123_456_789")
        .env(PREPASS_SESSION_ENV, &corrupt_session)
        .env("MOCK_RUSTC_LOG", &log_path)
        .output()
        .expect("invoke cargo-instrument with corrupted session");

    assert!(
        output.status.success(),
        "corrupted session should safely execute rustc without crash"
    );

    let recorded_args = read_mock_args(&log_path);
    assert!(
        !recorded_args.is_empty(),
        "mock rustc must be executed even with corrupt session"
    );
    assert!(
        !recorded_args
            .iter()
            .any(|arg| arg.starts_with("dependency=")),
        "corrupted session must not inject search paths"
    );
}

#[test]
fn test_prepass_wrapper_stale_fingerprint_omits_search_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (source_file, out_dir) = setup_test_crate(temp.path());
    let (_plan, session_file, otel_dir) = setup_session_with_otel(temp.path(), &source_file);
    let mock_rustc = get_mock_rustc();
    let log_path = temp.path().join("mock_rustc.log");

    // Pass a different fingerprint in PREPASS_FINGERPRINT_ENV
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .arg(&mock_rustc)
        .args([
            "--crate-name",
            "test_crate",
            source_file.to_str().unwrap(),
            "--crate-type",
            "lib",
            "--out-dir",
            out_dir.to_str().unwrap(),
            "-C",
            "opt-level=3",
        ])
        .env(WRAPPER_MODE_ENV, "1")
        .env(PREPASS_TOKEN_ENV, "prepass_sess123_456_789")
        .env(PREPASS_SESSION_ENV, &session_file)
        .env(PREPASS_FINGERPRINT_ENV, "mismatched_stale_fingerprint_9999")
        .env("MOCK_RUSTC_LOG", &log_path)
        .output()
        .expect("invoke cargo-instrument with stale fingerprint");

    assert!(output.status.success());
    let recorded_args = read_mock_args(&log_path);
    assert!(!recorded_args.is_empty(), "mock rustc must be executed");

    let expected_flag = format!("dependency={}", otel_dir.display());
    assert!(
        !recorded_args.iter().any(|arg| arg == &expected_flag),
        "stale fingerprint must reject session and not inject search paths"
    );
}

#[test]
fn test_prepass_wrapper_missing_fingerprint_omits_search_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (source_file, out_dir) = setup_test_crate(temp.path());
    let (_plan, session_file, otel_dir) = setup_session_with_otel(temp.path(), &source_file);
    let mock_rustc = get_mock_rustc();
    let log_path = temp.path().join("mock_rustc.log");
    let source_before = fs::read_to_string(&source_file).unwrap();

    // Valid session present, but PREPASS_FINGERPRINT_ENV is completely absent.
    // Must conservatively reject session artifact injection, execute plain rustc without search paths,
    // write no stamps, and never fall through to normal instrumentation.
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .arg(&mock_rustc)
        .args([
            "--crate-name",
            "test_crate",
            source_file.to_str().unwrap(),
            "--crate-type",
            "lib",
            "--out-dir",
            out_dir.to_str().unwrap(),
            "-C",
            "opt-level=3",
        ])
        .env(WRAPPER_MODE_ENV, "1")
        .env(PREPASS_TOKEN_ENV, "prepass_sess123_456_789")
        .env(PREPASS_SESSION_ENV, &session_file)
        .env_remove(PREPASS_FINGERPRINT_ENV)
        .env("MOCK_RUSTC_LOG", &log_path)
        .output()
        .expect("invoke cargo-instrument with missing fingerprint");

    assert!(
        output.status.success(),
        "missing fingerprint in pre-pass should succeed with plain rustc"
    );
    let recorded_args = read_mock_args(&log_path);
    assert!(!recorded_args.is_empty(), "mock rustc must be executed");

    let expected_flag = format!("dependency={}", otel_dir.display());
    assert!(
        !recorded_args.iter().any(|arg| arg == &expected_flag),
        "missing fingerprint must conservatively reject session and not inject search paths"
    );

    // Invariant: source file is unmodified
    let source_after = fs::read_to_string(&source_file).unwrap();
    assert_eq!(
        source_before, source_after,
        "pre-pass must not transform source code"
    );

    // Invariant: zero stamps written
    let stamps: Vec<_> = fs::read_dir(&out_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .contains(".cargo-instrument-")
        })
        .collect();
    assert!(
        stamps.is_empty(),
        "pre-pass with missing fingerprint must not write stamps"
    );
}

#[test]
fn test_prepass_wrapper_empty_fingerprint_omits_search_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (source_file, out_dir) = setup_test_crate(temp.path());
    let (_plan, session_file, otel_dir) = setup_session_with_otel(temp.path(), &source_file);
    let mock_rustc = get_mock_rustc();
    let log_path = temp.path().join("mock_rustc.log");
    let source_before = fs::read_to_string(&source_file).unwrap();

    // Valid session present, but PREPASS_FINGERPRINT_ENV is empty string.
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .arg(&mock_rustc)
        .args([
            "--crate-name",
            "test_crate",
            source_file.to_str().unwrap(),
            "--crate-type",
            "lib",
            "--out-dir",
            out_dir.to_str().unwrap(),
            "-C",
            "opt-level=3",
        ])
        .env(WRAPPER_MODE_ENV, "1")
        .env(PREPASS_TOKEN_ENV, "prepass_sess123_456_789")
        .env(PREPASS_SESSION_ENV, &session_file)
        .env(PREPASS_FINGERPRINT_ENV, "")
        .env("MOCK_RUSTC_LOG", &log_path)
        .output()
        .expect("invoke cargo-instrument with empty fingerprint");

    assert!(
        output.status.success(),
        "empty fingerprint in pre-pass should succeed with plain rustc"
    );
    let recorded_args = read_mock_args(&log_path);
    assert!(!recorded_args.is_empty(), "mock rustc must be executed");

    let expected_flag = format!("dependency={}", otel_dir.display());
    assert!(
        !recorded_args.iter().any(|arg| arg == &expected_flag),
        "empty fingerprint must conservatively reject session and not inject search paths"
    );

    // Invariant: source file is unmodified
    let source_after = fs::read_to_string(&source_file).unwrap();
    assert_eq!(
        source_before, source_after,
        "pre-pass must not transform source code"
    );

    // Invariant: zero stamps written
    let stamps: Vec<_> = fs::read_dir(&out_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .contains(".cargo-instrument-")
        })
        .collect();
    assert!(
        stamps.is_empty(),
        "pre-pass with empty fingerprint must not write stamps"
    );
}

#[test]
fn test_prepass_wrapper_invalid_token_fails_fast_without_invoking_compiler() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (source_file, out_dir) = setup_test_crate(temp.path());
    let mock_rustc = get_mock_rustc();
    let log_path = temp.path().join("mock_rustc.log");

    // Invalid token format (e.g. malformed prefix / structure)
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .arg(&mock_rustc)
        .args([
            "--crate-name",
            "test_crate",
            source_file.to_str().unwrap(),
            "--crate-type",
            "lib",
            "--out-dir",
            out_dir.to_str().unwrap(),
        ])
        .env(WRAPPER_MODE_ENV, "1")
        .env(PREPASS_TOKEN_ENV, "malformed_token_without_prepass_prefix")
        .env("MOCK_RUSTC_LOG", &log_path)
        .output()
        .expect("invoke cargo-instrument with invalid token");

    assert_eq!(
        output.status.code(),
        Some(1),
        "invalid token must immediately fail with exit code 1"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid pre-pass authorization token"),
        "stderr must report invalid pre-pass token error: {stderr}"
    );

    // Mock rustc must NEVER have been called
    assert!(
        !log_path.exists(),
        "compiler must never be invoked when token is invalid"
    );
}

#[test]
fn test_prepass_wrapper_compiler_failure_propagates_exit_code() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (source_file, out_dir) = setup_test_crate(temp.path());
    let mock_rustc = get_mock_rustc();
    let log_path = temp.path().join("mock_rustc.log");

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .arg(&mock_rustc)
        .args([
            "--crate-name",
            "test_crate",
            source_file.to_str().unwrap(),
            "--crate-type",
            "lib",
            "--out-dir",
            out_dir.to_str().unwrap(),
        ])
        .env(WRAPPER_MODE_ENV, "1")
        .env(PREPASS_TOKEN_ENV, "prepass_sess123_456_789")
        .env("MOCK_RUSTC_EXIT_CODE", "42")
        .env("MOCK_RUSTC_LOG", &log_path)
        .output()
        .expect("invoke cargo-instrument with failing compiler");

    assert_eq!(
        output.status.code(),
        Some(42),
        "compiler exit code 42 must be propagated by wrapper"
    );

    // Mock rustc was called
    let recorded_args = read_mock_args(&log_path);
    assert!(!recorded_args.is_empty(), "mock rustc must have run");

    // No stamps written
    let stamps: Vec<_> = fs::read_dir(&out_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .contains(".cargo-instrument-")
        })
        .collect();
    assert!(
        stamps.is_empty(),
        "failing compilation must not write stamps"
    );
}
