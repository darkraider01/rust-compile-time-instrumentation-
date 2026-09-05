use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use thiserror::Error;

use crate::ast::analyze_source_file;
use crate::candidate::{Candidate, DiscoveryReport};
use crate::discovery::{CrateInvocation, DiscoveryError};
use crate::transform::{
    paths_are_identical, transform_source_file_scoped_with_emitter, Emitter, NativeOtelEmitter,
    SentinelEmitter, SkipReason,
};

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
    pub rustc_binary: PathBuf,
    pub rustc_args: Vec<String>,
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

    pub fn from_env_and_args() -> Result<Self, WrapperError> {
        let raw_args: Vec<String> = env::args().collect();
        Self::from_args(&raw_args)
    }
}

/// Execute the compiler wrapper:
/// 1. Check recursion guard.
/// 2. Classify the invocation, verify target-dir isolation, and discover source candidates if eligible.
/// 3. H1: If eligible candidates exist, mirror and surgically transform the crate into an isolated directory
///    and rewrite the root source argument to point to the instrumented mirror (without modifying original source files).
/// 4. S11: If analysis, mirroring, or transformation fails, warn and compile unmodified.
/// 5. Delegate to the real `rustc` compiler.
/// 6. Return exit status code.
pub fn run_wrapper(config: &WrapperConfig) -> Result<i32, WrapperError> {
    // 1. Recursion check for direct sub-processes
    let is_nested_invocation = env::var(RECURSION_GUARD_ENV).is_ok();
    let mut args_to_run = config.rustc_args.clone();

    if !is_nested_invocation {
        // 2. Parse and classify compiler invocation
        if let Ok(invocation) = CrateInvocation::parse(&config.rustc_args) {
            if invocation.unit.is_eligible_for_analysis() {
                // H2: Check target dir isolation (ADR-004) when wrapper is invoked directly (not via CLI)
                let wrapper_mode = env::var("CARGO_INSTRUMENT_WRAPPER_MODE").is_ok();
                if !wrapper_mode {
                    if let Some(out_dir) = invocation.unit.out_dir() {
                        let out_dir_str = out_dir.to_string_lossy();
                        if !out_dir_str.contains("instrumented") {
                            eprintln!(
                                "warning: cargo-instrument: compilation is not using an isolated target directory \
                                (expected 'target/instrumented'). Build cache isolation (ADR-004) is inactive."
                            );
                        }
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

                            // S11 / C3: Dependency check before transformation
                            let has_otel = invocation.unit.has_opentelemetry();
                            let use_sentinel = env::var("CARGO_INSTRUMENT_SENTINEL_MODE").is_ok();
                            let native_otel_enforced =
                                env::var("CARGO_INSTRUMENT_NATIVE_OTEL").is_ok();

                            // H1: Splicing pipeline integration
                            if !report.candidates.is_empty() {
                                if !use_sentinel && native_otel_enforced && !has_otel {
                                    eprintln!(
                                        "warning: cargo-instrument: crate '{crate_name}' does not depend on 'opentelemetry'. \
                                        Skipping instrumentation per S11 fail-open."
                                    );
                                } else {
                                    let emitter: Box<dyn Emitter> = if use_sentinel {
                                        Box::new(SentinelEmitter)
                                    } else if has_otel {
                                        Box::new(NativeOtelEmitter::new(crate_name))
                                    } else {
                                        Box::new(SentinelEmitter)
                                    };

                                    match mirror_and_transform_crate_sources(
                                        &current_dir,
                                        source_file,
                                        crate_name,
                                        invocation.unit.out_dir(),
                                        &report,
                                        emitter.as_ref(),
                                        config.debug_output,
                                    ) {
                                        Ok(new_root) => {
                                            // Replace root source file argument with mirrored instrumented root
                                            for arg in &mut args_to_run {
                                                let p = Path::new(arg);
                                                if p == source_file
                                                    || p == resolved_path
                                                    || paths_are_identical(p, &resolved_path)
                                                {
                                                    *arg = new_root.to_string_lossy().to_string();
                                                    break;
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            // S11 fail-open per crate: log warning and compile unmodified
                                            eprintln!(
                                                "warning: cargo-instrument: failed to instrument '{crate_name}': {e}. \
                                                Compiling original source unmodified per S11."
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // 3. Delegate to the real rustc
    let status = execute_real_rustc(&config.rustc_binary, &args_to_run)?;

    Ok(status.code().unwrap_or(1))
}

/// Mirror and surgically transform all source files for an eligible crate compilation unit.
///
/// Places instrumented source files into an isolated directory under `--out-dir` (or temporary
/// directory if `--out-dir` is unspecified), leaving original source files 100% untouched.
/// Returns the updated root source file path to be passed to `rustc`.
fn mirror_and_transform_crate_sources(
    current_dir: &Path,
    source_file: &Path,
    crate_name: &str,
    out_dir: Option<&Path>,
    report: &DiscoveryReport,
    emitter: &dyn Emitter,
    debug_output: bool,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    // 1. Determine destination mirror directory
    let mirror_base = if let Some(out) = out_dir {
        if out.is_absolute() {
            out.join("instrumented_sources").join(crate_name)
        } else {
            current_dir
                .join(out)
                .join("instrumented_sources")
                .join(crate_name)
        }
    } else {
        std::env::temp_dir().join(format!(
            "cargo_instrument_{}_{}",
            crate_name,
            std::process::id()
        ))
    };

    // 2. Group candidates by their exact canonical source file path (C1)
    let mut candidates_by_file: HashMap<PathBuf, Vec<Candidate>> = HashMap::new();
    for c in &report.candidates {
        let canon = c
            .source_file
            .canonicalize()
            .unwrap_or_else(|_| c.source_file.clone());
        candidates_by_file.entry(canon).or_default().push(c.clone());
    }

    // 3. Identify directory containing source tree to mirror
    let resolved_source = if source_file.is_absolute() {
        source_file.to_path_buf()
    } else {
        current_dir.join(source_file)
    };

    let scan_dir = if let Some(parent) = resolved_source.parent() {
        parent.to_path_buf()
    } else {
        current_dir.to_path_buf()
    };

    if scan_dir.exists() && scan_dir.is_dir() {
        mirror_dir_recursive(
            &scan_dir,
            current_dir,
            &mirror_base,
            &candidates_by_file,
            emitter,
            crate_name,
            debug_output,
        )?;
    }

    // 4. Also mirror any candidate files that were out-of-line / outside scan_dir (e.g. #[path = "..."])
    for (canon_path, file_candidates) in &candidates_by_file {
        let rel = if let Ok(rel) = canon_path.strip_prefix(current_dir) {
            Some(rel.to_path_buf())
        } else if let Ok(canon_curr) = current_dir.canonicalize() {
            canon_path
                .strip_prefix(&canon_curr)
                .ok()
                .map(|r| r.to_path_buf())
        } else {
            None
        };
        if let Some(rel) = rel {
            let dest = mirror_base.join(&rel);
            if !dest.exists() {
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)?;
                }
                let plan = transform_source_file_scoped_with_emitter(
                    canon_path,
                    &dest,
                    file_candidates,
                    emitter,
                )?;
                if debug_output {
                    let deferred = plan
                        .skipped
                        .iter()
                        .filter(|s| matches!(s.reason, SkipReason::AsyncDeferred))
                        .count();
                    if deferred > 0 {
                        eprintln!(
                            "[cargo-instrument PID={} crate={}] deferred {} async candidates to P1.6",
                            std::process::id(),
                            crate_name,
                            deferred,
                        );
                    }
                }
            }
        }
    }

    // 5. Compute new_root inside mirror_base (guarding against absolute source_file path collapse)
    let rel_source = if let Ok(rel) = resolved_source.strip_prefix(current_dir) {
        rel.to_path_buf()
    } else if let (Ok(canon_source), Ok(canon_curr)) =
        (resolved_source.canonicalize(), current_dir.canonicalize())
    {
        if let Ok(rel) = canon_source.strip_prefix(&canon_curr) {
            rel.to_path_buf()
        } else {
            source_file
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("main.rs"))
        }
    } else if source_file.is_relative() {
        source_file.to_path_buf()
    } else {
        source_file
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("main.rs"))
    };

    let new_root = mirror_base.join(&rel_source);

    // Hard invariant checks: new_root must reside strictly inside mirror_base and must never collapse to original source
    if !new_root.starts_with(&mirror_base) || paths_are_identical(&new_root, &resolved_source) {
        return Err(format!(
            "Mirrored root source '{}' invalid or escaped mirror base '{}'",
            new_root.display(),
            mirror_base.display()
        )
        .into());
    }

    if !new_root.exists() {
        return Err(format!(
            "Mirrored root source file not found at '{}'",
            new_root.display()
        )
        .into());
    }

    if debug_output {
        eprintln!(
            "[cargo-instrument PID={} crate={}] transformed {} candidates into mirror {}",
            std::process::id(),
            crate_name,
            report.candidates.len(),
            mirror_base.display()
        );
    }

    Ok(new_root)
}

/// Recursively mirror and transform Rust source files.
fn mirror_dir_recursive(
    dir: &Path,
    current_dir: &Path,
    mirror_base: &Path,
    candidates_by_file: &HashMap<PathBuf, Vec<Candidate>>,
    emitter: &dyn Emitter,
    crate_name: &str,
    debug_output: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            mirror_dir_recursive(
                &path,
                current_dir,
                mirror_base,
                candidates_by_file,
                emitter,
                crate_name,
                debug_output,
            )?;
        } else if path.is_file() && path.extension().is_some_and(|e| e == "rs") {
            let rel = if let Ok(r) = path.strip_prefix(current_dir) {
                r.to_path_buf()
            } else if let (Ok(canon_path), Ok(canon_curr)) =
                (path.canonicalize(), current_dir.canonicalize())
            {
                if let Ok(r) = canon_path.strip_prefix(&canon_curr) {
                    r.to_path_buf()
                } else {
                    continue;
                }
            } else {
                continue;
            };
            let dest = mirror_base.join(&rel);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
            if let Some(file_candidates) = candidates_by_file.get(&canon) {
                let plan = transform_source_file_scoped_with_emitter(
                    &path,
                    &dest,
                    file_candidates,
                    emitter,
                )?;
                if debug_output {
                    let deferred = plan
                        .skipped
                        .iter()
                        .filter(|s| matches!(s.reason, SkipReason::AsyncDeferred))
                        .count();
                    if deferred > 0 {
                        eprintln!(
                            "[cargo-instrument PID={} crate={}] deferred {} async candidates to P1.6",
                            std::process::id(),
                            crate_name,
                            deferred,
                        );
                    }
                }
            } else {
                fs::copy(&path, &dest)?;
            }
        }
    }
    Ok(())
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
