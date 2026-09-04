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

/// Execute the compiler wrapper:
/// 1. Check recursion guard.
/// 2. Classify the invocation and discover source candidates if eligible.
/// 3. Delegate to the real `rustc` compiler.
/// 4. Return exit status code.
pub fn run_wrapper(config: &WrapperConfig) -> Result<i32, WrapperError> {
    // 1. Recursion check
    let is_nested_invocation = env::var(RECURSION_GUARD_ENV).is_ok();

    if !is_nested_invocation {
        // 2. Parse and classify compiler invocation
        if let Ok(invocation) = CrateInvocation::parse(&config.rustc_args) {
            if invocation.unit.is_eligible_for_analysis() {
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
                                eprintln!("{}", report.format_debug());
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
