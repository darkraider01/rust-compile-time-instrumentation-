use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use thiserror::Error;

use crate::ast::{analyze_source_file, check_application_preflight};
use crate::candidate::{Candidate, DiscoveryReport, UnsafePolicy};
use crate::discovery::{CrateInvocation, CrateRole, DiscoveryError};
use crate::session::SessionPlan;
use crate::transform::{
    paths_are_identical, transform_source_file_scoped_with_emitter, Emitter, NativeOtelEmitter,
    SentinelEmitter, SkipReason, TrampolineEmitter,
};
use crate::unit::UnitId;

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
    let mut mirrored_info: Option<(MirroredCrateInfo, PathBuf)> = None;

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
                    let role = invocation.unit.role(&current_dir);
                    let unit_id = invocation
                        .unit
                        .unit_id()
                        .unwrap_or_else(|| UnitId::from_crate_name(crate_name));

                    // Load session policy once per build session
                    let session_plan =
                        SessionPlan::load_or_create(&current_dir, invocation.unit.out_dir());

                    // Registry dependency opt-in gate (§12.1, §12.3):
                    let registry_enabled = env::var("CARGO_INSTRUMENT_REGISTRY")
                        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                        .unwrap_or(false);

                    let is_host_only = session_plan.is_host_only(crate_name);

                    let should_skip = if invocation.unit.is_telemetry_or_tool_crate() {
                        true
                    } else if is_host_only {
                        if config.debug_output {
                            eprintln!(
                                "[cargo-instrument PID={} crate={}] skipping host-only dependency",
                                std::process::id(),
                                crate_name
                            );
                        }
                        true
                    } else if role == CrateRole::RegistryDependency {
                        !registry_enabled
                    } else {
                        false
                    };

                    if !should_skip {
                        let resolved_path = if source_file.is_absolute() {
                            source_file.to_path_buf()
                        } else {
                            current_dir.join(source_file)
                        };

                        if resolved_path.exists() {
                            match analyze_source_file(crate_name, &resolved_path) {
                                Ok(report) => {
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
                                    let has_otel_shim = invocation.unit.has_otel_shim();
                                    let use_sentinel =
                                        env::var("CARGO_INSTRUMENT_SENTINEL_MODE").is_ok();
                                    let native_otel_enforced =
                                        env::var("CARGO_INSTRUMENT_NATIVE_OTEL").is_ok();

                                    // Preflight check for application crates declaring otel-shim (G5 / S11)
                                    // Downgraded from hard-exit to fail-open warning per P2.1 DoD #7
                                    let mut preflight_failed = false;
                                    if has_otel_shim {
                                        if let Err(msg) =
                                            check_application_preflight(crate_name, &resolved_path)
                                        {
                                            eprintln!(
                                                "warning: cargo-instrument: preflight check for application crate '{crate_name}' failed: {msg}. \
                                                Compiling unmodified per S11 fail-open."
                                            );
                                            preflight_failed = true;
                                        }
                                    }

                                    // H1: Splicing pipeline integration
                                    if !report.candidates.is_empty() {
                                        let emitter: Option<Box<dyn Emitter>> = if preflight_failed
                                        {
                                            None
                                        } else if report.has_colliding_symbols {
                                            eprintln!(
                                                "warning: cargo-instrument: crate '{crate_name}' exports an ABI symbol conflicting with otel-shim. \
                                                Skipping instrumentation per S11 fail-open."
                                            );
                                            None
                                        } else if report.is_no_std {
                                            eprintln!(
                                                "warning: cargo-instrument: crate '{crate_name}' specifies `#![no_std]`. \
                                                Skipping instrumentation per §12.3."
                                            );
                                            None
                                        } else if report.unsafe_policy == UnsafePolicy::Forbidden {
                                            eprintln!(
                                                "warning: cargo-instrument: crate '{crate_name}' specifies `#![forbid(unsafe_code)]`. \
                                                Skipping instrumentation per R26."
                                            );
                                            None
                                        } else if use_sentinel {
                                            Some(Box::new(SentinelEmitter))
                                        } else if role == CrateRole::Application {
                                            if has_otel {
                                                Some(Box::new(NativeOtelEmitter::new(crate_name)))
                                            } else if native_otel_enforced {
                                                eprintln!(
                                                    "warning: cargo-instrument: crate '{crate_name}' does not depend on 'opentelemetry'. \
                                                    Skipping instrumentation per S11 fail-open."
                                                );
                                                None
                                            } else {
                                                Some(Box::new(SentinelEmitter))
                                            }
                                        } else {
                                            // Tier 2: Non-application crate
                                            // G4 link provider gate: verify otel-shim is reachable in the build graph
                                            if !session_plan.has_otel_shim_provider() {
                                                eprintln!(
                                                    "warning: cargo-instrument: no otel-shim provider found in build graph for '{crate_name}'. \
                                                    Skipping instrumentation per S11 fail-open."
                                                );
                                                None
                                            } else {
                                                Some(Box::new(TrampolineEmitter::new(
                                                    crate_name,
                                                    invocation.unit.edition().map(String::from),
                                                    report.unsafe_policy,
                                                )))
                                            }
                                        };

                                        if let Some(emitter) = emitter {
                                            match mirror_and_transform_crate_sources(
                                                &current_dir,
                                                source_file,
                                                crate_name,
                                                &unit_id,
                                                invocation.unit.extra_filename(),
                                                invocation.unit.out_dir(),
                                                &report,
                                                emitter.as_ref(),
                                                config.debug_output,
                                            ) {
                                                Ok(info) => {
                                                    // Replace root source file argument with mirrored instrumented root
                                                    for arg in &mut args_to_run {
                                                        let p = Path::new(arg);
                                                        if p == source_file
                                                            || p == resolved_path
                                                            || paths_are_identical(
                                                                p,
                                                                &resolved_path,
                                                            )
                                                        {
                                                            *arg = info
                                                                .new_root
                                                                .to_string_lossy()
                                                                .to_string();
                                                            break;
                                                        }
                                                    }
                                                    if let Some(out) = invocation.unit.out_dir() {
                                                        let out_dir = if out.is_absolute() {
                                                            out.to_path_buf()
                                                        } else {
                                                            current_dir.join(out)
                                                        };
                                                        mirrored_info = Some((info, out_dir));
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
                                Err(e) => {
                                    eprintln!(
                                        "warning: cargo-instrument: failed to analyze '{crate_name}': {e}. \
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

    // 3. Delegate to the real rustc
    let status = execute_real_rustc(&config.rustc_binary, &args_to_run)?;

    if status.success() {
        if let Some((info, out_dir)) = mirrored_info {
            remap_dep_info_files(&out_dir, &info);
        }
    }

    Ok(status.code().unwrap_or(1))
}

#[derive(Debug, Clone)]
pub struct MirroredCrateInfo {
    pub new_root: PathBuf,
    pub mirror_base: PathBuf,
    pub base_rel_dir: PathBuf,
    pub is_in_tree: bool,
    pub source_was_relative: bool,
    pub crate_name: String,
    pub extra_filename: Option<String>,
}

/// Mirror and surgically transform all source files for an eligible crate compilation unit.
///
/// Places instrumented source files into an isolated directory under `--out-dir` (or temporary
/// directory if `--out-dir` is unspecified), leaving original source files 100% untouched.
/// Returns the updated root source file path to be passed to `rustc`.
#[allow(clippy::too_many_arguments)]
fn mirror_and_transform_crate_sources(
    current_dir: &Path,
    source_file: &Path,
    crate_name: &str,
    unit_id: &UnitId,
    extra_filename: Option<&str>,
    out_dir: Option<&Path>,
    report: &DiscoveryReport,
    emitter: &dyn Emitter,
    debug_output: bool,
) -> Result<MirroredCrateInfo, Box<dyn std::error::Error>> {
    // 1. Determine destination mirror directory re-keyed on UnitId
    let mirror_dir_name = unit_id.dir_name();
    let mirror_base = if let Some(out) = out_dir {
        if out.is_absolute() {
            out.join("instrumented_sources").join(&mirror_dir_name)
        } else {
            current_dir
                .join(out)
                .join("instrumented_sources")
                .join(&mirror_dir_name)
        }
    } else {
        std::env::temp_dir().join(format!(
            "cargo_instrument_{}_{}",
            mirror_dir_name,
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

    // Distinguish in-tree vs out-of-tree sources (M2):
    // For in-tree files, relativize against current_dir preserving H1 guarantees.
    // For out-of-tree dependencies (e.g. external path deps), relativize against scan_dir.
    let is_in_tree = resolved_source.starts_with(current_dir)
        || (resolved_source.is_relative() && !resolved_source.starts_with(".."))
        || matches!(
            (resolved_source.canonicalize(), current_dir.canonicalize()),
            (Ok(s), Ok(c)) if s.starts_with(&c)
        );

    let base_rel_dir = if is_in_tree { current_dir } else { &scan_dir };

    // Sandboxing invariant (A16): if any candidate file escapes base_rel_dir (e.g. via #[path = "..."]),
    // fail open per S11 rather than risk out-of-sandbox writes or broken relative paths.
    for canon_path in candidates_by_file.keys() {
        let is_inside_base = canon_path.starts_with(base_rel_dir)
            || matches!(
                (canon_path.canonicalize(), base_rel_dir.canonicalize()),
                (Ok(c), Ok(b)) if c.starts_with(&b)
            );
        if !is_inside_base {
            return Err(format!(
                "Source file '{}' referenced via #[path] escapes crate root '{}'",
                canon_path.display(),
                base_rel_dir.display()
            )
            .into());
        }
    }

    if scan_dir.exists() && scan_dir.is_dir() {
        mirror_dir_recursive(
            &scan_dir,
            base_rel_dir,
            &mirror_base,
            &candidates_by_file,
            emitter,
            crate_name,
            debug_output,
        )?;
    }

    // 4. Also mirror any candidate files that were out-of-line / outside scan_dir (e.g. #[path = "..."])
    for (canon_path, file_candidates) in &candidates_by_file {
        let rel = if let Ok(rel) = canon_path.strip_prefix(base_rel_dir) {
            Some(rel.to_path_buf())
        } else if let Ok(canon_base) = base_rel_dir.canonicalize() {
            canon_path
                .strip_prefix(&canon_base)
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
                            "[cargo-instrument PID={} crate={}] deferred {} async candidates (emitter does not handle async)",
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
    let rel_source = if is_in_tree {
        if let Ok(rel) = resolved_source.strip_prefix(current_dir) {
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
        }
    } else {
        // Out-of-tree dependency: relativize against scan_dir (crate source root)
        if let Ok(rel) = resolved_source.strip_prefix(&scan_dir) {
            rel.to_path_buf()
        } else if let (Ok(canon_source), Ok(canon_scan)) =
            (resolved_source.canonicalize(), scan_dir.canonicalize())
        {
            if let Ok(rel) = canon_source.strip_prefix(&canon_scan) {
                rel.to_path_buf()
            } else {
                source_file
                    .file_name()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("lib.rs"))
            }
        } else {
            source_file
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("lib.rs"))
        }
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

    Ok(MirroredCrateInfo {
        new_root,
        mirror_base,
        base_rel_dir: base_rel_dir.to_path_buf(),
        is_in_tree,
        source_was_relative: source_file.is_relative(),
        crate_name: crate_name.to_string(),
        extra_filename: extra_filename.map(String::from),
    })
}

/// Recursively mirror and transform Rust source files.
fn mirror_dir_recursive(
    dir: &Path,
    base_rel_dir: &Path,
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
                base_rel_dir,
                mirror_base,
                candidates_by_file,
                emitter,
                crate_name,
                debug_output,
            )?;
        } else if path.is_file() && path.extension().is_some_and(|e| e == "rs") {
            let rel = if let Ok(r) = path.strip_prefix(base_rel_dir) {
                r.to_path_buf()
            } else if let (Ok(canon_path), Ok(canon_base)) =
                (path.canonicalize(), base_rel_dir.canonicalize())
            {
                if let Ok(r) = canon_path.strip_prefix(&canon_base) {
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
                            "[cargo-instrument PID={} crate={}] deferred {} async candidates (emitter does not handle async)",
                            std::process::id(),
                            crate_name,
                            deferred,
                        );
                    }
                }
            } else {
                if dest.exists() {
                    if let (Ok(src_bytes), Ok(dest_bytes)) = (fs::read(&path), fs::read(&dest)) {
                        if src_bytes == dest_bytes {
                            continue;
                        }
                    }
                }
                // Atomic copy to temporary destination first
                let temp_dest = dest.with_extension(format!("tmp.{}", std::process::id()));
                fs::copy(&path, &temp_dest)?;
                if let Ok(metadata) = fs::metadata(&path) {
                    if let Ok(mtime) = metadata.modified() {
                        if let Ok(file) = fs::OpenOptions::new().write(true).open(&temp_dest) {
                            let times = fs::FileTimes::new().set_modified(mtime);
                            let _ = file.set_times(times);
                        }
                    }
                }
                if fs::rename(&temp_dest, &dest).is_err() {
                    let _ = fs::remove_file(&temp_dest);
                    fs::copy(&path, &dest)?;
                    if let Ok(metadata) = fs::metadata(&path) {
                        if let Ok(mtime) = metadata.modified() {
                            if let Ok(file) = fs::OpenOptions::new().write(true).open(&dest) {
                                let times = fs::FileTimes::new().set_modified(mtime);
                                let _ = file.set_times(times);
                            }
                        }
                    }
                }
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

/// Remap compiler-generated dep-info (.d) files to point back to original source paths.
///
/// Because `rustc` was invoked with the mirrored source root inside `<out-dir>/instrumented_sources/...`,
/// the generated dep-info (.d) file records dependencies on the mirrored files instead of original files.
/// Rewriting the .d file restores Cargo's ability to watch original source files for incremental changes
/// while avoiding false dirty rebuild triggers on repeat builds (A13).
fn remap_dep_info_files(out_dir: &Path, info: &MirroredCrateInfo) {
    let mirror_str = info.mirror_base.to_string_lossy();
    let mirror_str_fwd = mirror_str.replace('\\', "/");

    let replacement = if info.is_in_tree && info.source_was_relative {
        String::new()
    } else {
        info.base_rel_dir.to_string_lossy().to_string()
    };
    let replacement_fwd = replacement.replace('\\', "/");

    let remap_file = |path: &Path| {
        if let Ok(content) = fs::read_to_string(path) {
            if content.contains(&*mirror_str) || content.contains(&mirror_str_fwd) {
                let mirror_slash = format!("{}/", mirror_str_fwd.trim_end_matches('/'));
                let mirror_backslash = format!("{}\\", mirror_str.trim_end_matches('\\'));

                let rep_slash = if replacement_fwd.is_empty() {
                    String::new()
                } else {
                    format!("{}/", replacement_fwd.trim_end_matches('/'))
                };
                let rep_backslash = if replacement.is_empty() {
                    String::new()
                } else {
                    format!("{}\\", replacement.trim_end_matches('\\'))
                };

                let updated = content
                    .replace(&mirror_slash, &rep_slash)
                    .replace(&mirror_backslash, &rep_backslash)
                    .replace(&mirror_str_fwd, &replacement_fwd)
                    .replace(&*mirror_str, &replacement);

                let _ = fs::write(path, updated);
            }
        }
    };

    // Scoped remapping (G6): target this unit's own dep-info file directly
    let extra = info.extra_filename.as_deref().unwrap_or("");
    let candidate_names = [
        format!("{}{}.d", info.crate_name, extra),
        format!("lib{}{}.d", info.crate_name, extra),
        format!("{}.d", info.crate_name),
        format!("lib{}.d", info.crate_name),
    ];

    let mut found = false;
    for name in &candidate_names {
        let p = out_dir.join(name);
        if p.is_file() {
            remap_file(&p);
            found = true;
        }
    }

    if !found {
        if let Ok(entries) = fs::read_dir(out_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().is_some_and(|e| e == "d") {
                    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if file_name.starts_with(&info.crate_name)
                        || file_name.starts_with(&format!("lib{}", info.crate_name))
                    {
                        remap_file(&path);
                    }
                }
            }
        }
    }
}
