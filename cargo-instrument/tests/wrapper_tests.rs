use cargo_instrument::wrapper::{run_wrapper, WrapperConfig, RECURSION_GUARD_ENV};
use std::env;
use std::path::PathBuf;

#[test]
fn test_wrapper_config_parsing_from_cargo_args() {
    let args = vec![
        "target/debug/cargo-instrument.exe".to_string(),
        "rustc.exe".to_string(),
        "--crate-name".to_string(),
        "my_crate".to_string(),
        "src/lib.rs".to_string(),
    ];

    let config = WrapperConfig::from_args(&args).expect("config parsing should succeed");
    assert_eq!(config.rustc_binary, PathBuf::from("rustc.exe"));
    assert_eq!(
        config.rustc_args,
        vec!["--crate-name", "my_crate", "src/lib.rs"]
    );
}

#[test]
fn test_wrapper_argument_forwarding_and_exit_code() {
    // Invoke real rustc with -vV to verify argument forwarding and exit status 0
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let config = WrapperConfig {
        rustc_binary: PathBuf::from(rustc),
        rustc_args: vec!["-vV".to_string()],
        debug_output: false,
    };

    let exit_code = run_wrapper(&config).expect("run_wrapper should succeed");
    assert_eq!(exit_code, 0, "rustc -vV should exit with code 0");
}

#[test]
fn test_wrapper_failing_exit_code_propagation() {
    // Invoke rustc with an invalid flag to verify non-zero exit code propagation
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let config = WrapperConfig {
        rustc_binary: PathBuf::from(rustc),
        rustc_args: vec!["--invalid-flag-that-does-not-exist-12345".to_string()],
        debug_output: false,
    };

    let exit_code = run_wrapper(&config).expect("run_wrapper should return exit code");
    assert_ne!(
        exit_code, 0,
        "rustc should fail and exit code must be non-zero"
    );
}

#[test]
fn test_wrapper_recursion_guard() {
    // When CARGO_INSTRUMENT_ACTIVE is set, the wrapper must safely forward without recursing
    env::set_var(RECURSION_GUARD_ENV, "1");

    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let config = WrapperConfig {
        rustc_binary: PathBuf::from(rustc),
        rustc_args: vec!["-vV".to_string()],
        debug_output: false,
    };

    let exit_code = run_wrapper(&config).expect("run_wrapper with recursion guard should succeed");
    assert_eq!(exit_code, 0);

    env::remove_var(RECURSION_GUARD_ENV);
}
