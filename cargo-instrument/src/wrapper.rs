use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use thiserror::Error;

use crate::ast::analyze_source_file;
use crate::discovery::{CrateInvocation, DiscoveryError};

pub const RECURSION_GUARD_ENV: &str = "CARGO_INSTRUMENT_ACTIVE";
pub const DEBUG_ENV: &str = "INSTRUMENT_DEBUG";

#[derive(Debug, Error)]
pub enum WrapperError {
    #[error("No rustc command specified")]
    MissingRustc,
    #[error("Failed to execute real rustc '{path}': {source}")]
    SpawnRustc {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("Discovery error: {0}")]
    Discovery(#[from] DiscoveryError),
}

/// Execution configuration for the wrapper.
#[derive(Debug, Clone)]
pub struct WrapperConfig {
    /// Path to the real `rustc` compiler executable.
    pub rustc_binary: PathBuf,
    /// Arguments to forward to `rustc`.
    pub rustc_args: Vec<String>,
    /// Whether to print candidate reports to stderr.
    pub debug_output: bool,
}

impl WrapperConfig {
    /// Extract wrapper configuration from CLI arguments passed by Cargo.
    ///
    /// When invoked via `RUSTC_WRAPPER`, Cargo provides:
    /// `wrapper_binary <path-to-rustc> [rustc-arguments...]`
    pub fn from_args(args: &[String]) -> Result<Self, WrapperError> {
        if args.len() < 2 {
            // Check environment fallback
            let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
            let rustc_args = if args.len() == 1 {
                Vec::new()
            } else {
                args[1..].to_vec()
            };
            return Ok(WrapperConfig {
                rustc_binary: PathBuf::from(rustc),
                rustc_args,
                debug_output: env::var(DEBUG_ENV).map(|v| v != "0").unwrap_or(false),
            });
        }

        let rustc_binary = PathBuf::from(&args[1]);
        let rustc_args = args[2..].to_vec();
        let debug_output = env::var(DEBUG_ENV).map(|v| v != "0").unwrap_or(false);

        Ok(WrapperConfig {
            rustc_binary,
            rustc_args,
            debug_output,
        })
    }
}

/// ARCHITECTURAL NOTE ON BUILD SCRIPT RECURSION (DEFERRED GAP):
/// Setting an environment variable inside the wrapper process only affects direct child processes
/// spawned by this wrapper invocation. When Cargo compiles a `build.rs` script, this wrapper exits;
/// Cargo subsequently executes the compiled build script binary as its own direct child.
/// If that `build.rs` shells out to `cargo build`, it inherits RUSTC_WRAPPER from Cargo's environment,
/// leading to nested wrapper invocations.
/// In Phase 1 (analysis-only), the consequence of an unguarded nested build-script invocation is
/// redundant CPU usage and interleaved debug output, not code corruption.
/// We tag all debug output with PID and crate name to distinguish these invocations.
/// Full build-script recursion prevention via package-graph metadata filtering is explicitly
/// deferred to Phase 2, where byte-splicing makes strict isolation load-bearing.
///
/// Execute the compiler wrapper:
/// 1. Check recursion guard.
/// 2. Classify the invocation, verify target-dir isolation, and discover source candidates if eligible.
/// 3. Delegate to the real `rustc` compiler.
/// 4. Return exit status code.
pub fn run_wrapper(config: &WrapperConfig) -> Result<i32, WrapperError> {
    // 1. Recursion check for direct sub-processes
    let is_nested_invocation = env::var(RECURSION_GUARD_ENV).is_ok();

    if !is_nested_invocation {
        // 2. Parse and classify compiler invocation
        if let Ok(invocation) = CrateInvocation::parse(&config.rustc_args) {
            if invocation.unit.is_eligible_for_analysis() {
                // H2: Check target dir isolation (ADR-004) when wrapper is invoked directly (not via CLI)
                let wrapper_mode = env::var("CARGO_INSTRUMENT_WRAPPER_MODE").is_ok();
                if !wrapper_mode {
                    let out_dir = invocation.unit.out_dir().unwrap_or(Path::new(""));
                    let is_isolated = out_dir.to_string_lossy().contains("instrumented");
                    if !is_isolated {
                        eprintln!(
                            "warning: cargo-instrument: compilation is not using an isolated target directory \
                            (expected 'target/instrumented'). Build cache isolation (ADR-004) is inactive."
                        );
                    }
                }

                if let (Some(crate_name), Some(source_file)) =
                    (invocation.unit.crate_name(), invocation.unit.source_file())
                {
                    // Resolve relative source paths against current working directory
                    let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                    let resolved_path = if source_file.is_absolute() {
                        source_file.to_path_buf()
                    } else {
                        current_dir.join(source_file)
                    };

                    if resolved_path.exists() {
                        if let Ok(report) = analyze_source_file(crate_name, &resolved_path) {
                            if config.debug_output {
                                eprintln!(
                                    "[cargo-instrument PID={} crate={}]\n{}",
                                    std::process::id(),
                                    crate_name,
                                    report.format_debug()
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    // 3. Delegate to the real rustc
    let status = execute_real_rustc(&config.rustc_binary, &config.rustc_args)?;

    Ok(status.code().unwrap_or(1))
}

/// Forward execution to the real `rustc` binary with recursion guard set.
fn execute_real_rustc(rustc: &Path, args: &[String]) -> Result<ExitStatus, WrapperError> {
    let mut cmd = Command::new(rustc);
    cmd.args(args);
    cmd.env(RECURSION_GUARD_ENV, "1");

    cmd.status().map_err(|e| WrapperError::SpawnRustc {
        path: rustc.to_path_buf(),
        source: e,
    })
}
