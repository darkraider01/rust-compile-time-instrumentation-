use std::env;
use std::path::{Path, PathBuf};
use std::process::{self, Command};

use cargo_instrument::{
    analyze_source_file, run_wrapper, transform_source_file, transform_source_str, SessionPlan,
    UnitId, WrapperConfig, DEBUG_ENV, SESSION_ENV,
};

const WRAPPER_MODE_ENV: &str = "CARGO_INSTRUMENT_WRAPPER_MODE";
const DEFAULT_TARGET_DIR: &str = "target/instrumented";

fn main() {
    let args: Vec<String> = env::args().collect();

    // Determine if this process was invoked as RUSTC_WRAPPER or as the CLI
    if is_wrapper_invocation(&args) {
        let config = match WrapperConfig::from_args(&args) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("cargo-instrument wrapper error: {e}");
                process::exit(1);
            }
        };

        match run_wrapper(&config) {
            Ok(code) => process::exit(code),
            Err(e) => {
                eprintln!("cargo-instrument execution error: {e}");
                process::exit(1);
            }
        }
    } else {
        run_cli(&args);
    }
}

/// Detect whether the binary was invoked as a compiler wrapper by Cargo.
fn is_wrapper_invocation(args: &[String]) -> bool {
    // 1. Explicit wrapper mode environment variable
    if env::var(WRAPPER_MODE_ENV).is_ok() {
        return true;
    }

    if args.len() < 2 {
        return false;
    }

    // 2. If first arg is a known CLI subcommand or help flag, it is not a wrapper invocation
    let first = &args[1];
    if first == "instrument"
        || first == "analyze"
        || first == "transform"
        || first == "--help"
        || first == "-h"
        || first == "--version"
        || first == "-V"
    {
        return false;
    }

    // 3. When Cargo runs RUSTC_WRAPPER, args[1] is the path to rustc (or ends with rustc / rustc.exe)
    let path = Path::new(first);
    if let Some(stem) = path.file_stem() {
        if stem == "rustc" {
            return true;
        }
    }

    // Also check if args[1] is an executable file path and not a flag
    if !first.starts_with('-') && !first.ends_with(".rs") && path.exists() {
        return true;
    }

    false
}

/// Handle CLI entry point (`cargo instrument [OPTIONS] -- <cargo args...>`).
fn run_cli(args: &[String]) {
    // Handle invocation as `cargo instrument ...` where Cargo passes "instrument" as args[1]
    let cli_args: Vec<String> = if args.len() > 1 && args[1] == "instrument" {
        args[2..].to_vec()
    } else if args.len() > 1 {
        args[1..].to_vec()
    } else {
        Vec::new()
    };

    if cli_args.is_empty() || cli_args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

    if cli_args.iter().any(|a| a == "--version" || a == "-V") {
        println!("cargo-instrument {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    // Subcommand: `analyze <file.rs>`
    if cli_args[0] == "analyze" {
        if cli_args.len() < 2 {
            eprintln!("Usage: cargo instrument analyze <file.rs>");
            process::exit(1);
        }
        let file_path = PathBuf::from(&cli_args[1]);
        let crate_name = file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("crate");

        match analyze_source_file(crate_name, &file_path) {
            Ok(report) => {
                println!("{}", report.format_debug());
                process::exit(0);
            }
            Err(e) => {
                eprintln!("Error analyzing {}: {e}", file_path.display());
                process::exit(1);
            }
        }
    }

    // Subcommand: `transform <file.rs> [--output <dest.rs>]`
    if cli_args[0] == "transform" {
        if cli_args.len() < 2 {
            eprintln!("Usage: cargo instrument transform <file.rs> [--output <destination.rs>]");
            process::exit(1);
        }
        let file_path = PathBuf::from(&cli_args[1]);
        let mut output_path = None;
        let mut idx = 2;
        while idx < cli_args.len() {
            if cli_args[idx] == "--output" || cli_args[idx] == "-o" {
                if idx + 1 < cli_args.len() {
                    output_path = Some(PathBuf::from(&cli_args[idx + 1]));
                    idx += 2;
                    continue;
                } else {
                    eprintln!("Error: --output requires a path argument");
                    process::exit(1);
                }
            } else if cli_args[idx].starts_with("--output=") {
                let val = &cli_args[idx]["--output=".len()..];
                output_path = Some(PathBuf::from(val));
                idx += 1;
                continue;
            }
            idx += 1;
        }

        let crate_name = file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("crate");

        let report = match analyze_source_file(crate_name, &file_path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Error analyzing {}: {e}", file_path.display());
                process::exit(1);
            }
        };

        if let Some(dest) = output_path {
            match transform_source_file(&file_path, &dest, &report.candidates) {
                Ok(_) => process::exit(0),
                Err(e) => {
                    eprintln!("Error transforming {}: {e}", file_path.display());
                    process::exit(1);
                }
            }
        } else {
            let source_bytes = match std::fs::read(&file_path) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("Error reading {}: {e}", file_path.display());
                    process::exit(1);
                }
            };
            let source_text = match String::from_utf8(source_bytes) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Source {} is not valid UTF-8: {e}", file_path.display());
                    process::exit(1);
                }
            };
            match transform_source_str(&source_text, &report.candidates) {
                Ok(transformed) => {
                    print!("{transformed}");
                    process::exit(0);
                }
                Err(e) => {
                    eprintln!("Error transforming {}: {e}", file_path.display());
                    process::exit(1);
                }
            }
        }
    }

    // Otherwise, forward to Cargo with RUSTC_WRAPPER set and isolated --target-dir wired (ADR-004)
    execute_cargo_with_wrapper(&cli_args);
}

struct CliInvocation {
    cargo_args: Vec<String>,
    app_args: Vec<String>,
    has_app_args_separator: bool,
}

fn parse_cli_invocation(raw_args: &[String]) -> CliInvocation {
    let mut cargo_args = Vec::new();
    let mut app_args = Vec::new();
    let mut has_app_args_separator = false;

    let mut iter = raw_args.iter().peekable();
    if iter.peek().map(|s| s.as_str()) == Some("--") {
        iter.next();
    }

    let mut in_app_args = false;
    for arg in iter {
        if in_app_args {
            app_args.push(arg.clone());
        } else if arg == "--" {
            in_app_args = true;
            has_app_args_separator = true;
        } else {
            cargo_args.push(arg.clone());
        }
    }

    if cargo_args.is_empty() {
        cargo_args.push("build".to_string());
    }

    CliInvocation {
        cargo_args,
        app_args,
        has_app_args_separator,
    }
}

/// Execute Cargo with RUSTC_WRAPPER pointing to this executable and isolated --target-dir set.
fn execute_cargo_with_wrapper(args: &[String]) {
    let current_exe = match env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("Failed to locate current executable: {e}");
            process::exit(1);
        }
    };

    let mut cargo_cmd = Command::new("cargo");

    // Set RUSTC_WRAPPER to current executable
    cargo_cmd.env("RUSTC_WRAPPER", &current_exe);
    cargo_cmd.env(WRAPPER_MODE_ENV, "1");

    // If INSTRUMENT_DEBUG is not set, set it by default so debug output is visible
    if env::var(DEBUG_ENV).is_err() {
        cargo_cmd.env(DEBUG_ENV, "1");
    }

    let mut invocation = parse_cli_invocation(args);

    // Check if user already provided --target-dir
    let has_target_dir = invocation
        .cargo_args
        .iter()
        .any(|a| a == "--target-dir" || a.starts_with("--target-dir="));

    // Append isolated --target-dir target/instrumented (ADR-004) if not already provided
    if !has_target_dir {
        invocation.cargo_args.push("--target-dir".to_string());
        invocation.cargo_args.push(DEFAULT_TARGET_DIR.to_string());
    }

    let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // Extract target directory from cargo_args or use DEFAULT_TARGET_DIR
    let target_dir_str = invocation
        .cargo_args
        .iter()
        .position(|a| a == "--target-dir")
        .and_then(|idx| invocation.cargo_args.get(idx + 1).cloned())
        .or_else(|| {
            invocation
                .cargo_args
                .iter()
                .find(|a| a.starts_with("--target-dir="))
                .map(|a| a.trim_start_matches("--target-dir=").to_string())
        })
        .unwrap_or_else(|| DEFAULT_TARGET_DIR.to_string());

    let target_dir = PathBuf::from(target_dir_str);
    let resolved_target_dir = if target_dir.is_absolute() {
        target_dir
    } else {
        current_dir.join(target_dir)
    };

    // Precompute SessionPlan once using the invocation's resolution flags.  In particular, an
    // offline Cargo command must never have its orchestration metadata probe reach the network.
    // and export via CARGO_INSTRUMENT_SESSION for all wrapper child processes (D3 / D4).
    let session_file = resolved_target_dir.join("cargo_instrument_session.json");
    match build_session_plan(&invocation.cargo_args, &current_dir) {
        Ok(mut plan) => {
            // Native R-4 acquisition is deliberately limited to ordinary build/run.  The
            // pre-pass uses this exact target directory so Cargo's crate identities remain
            // compatible with the final wrapper-enabled build.  Unsupported commands retain
            // the established wrapper-only behavior.
            if supports_native_orchestration(&invocation.cargo_args) {
                match acquire_native_artifacts(
                    &invocation.cargo_args,
                    &resolved_target_dir,
                    &current_dir,
                    &mut plan,
                ) {
                    Ok(NativeAcquisition::Available) => {}
                    Ok(NativeAcquisition::Tier2Only(reason)) => {
                        eprintln!("warning: cargo-instrument: native orchestration disabled: {reason}. Tier-2/fail-open policy remains active.");
                    }
                    Err(e) => {
                        eprintln!("cargo-instrument orchestration error: {e}");
                        process::exit(1);
                    }
                }
            }
            if let Err(e) = plan.save_to_file(&session_file) {
                eprintln!("warning: cargo-instrument: failed to save session plan: {e}");
            }
            cargo_cmd.env(SESSION_ENV, &session_file);
        }
        Err(e) => {
            eprintln!(
                "warning: cargo-instrument: failed to precompute session plan from metadata ({e}). \
                 Continuing with wrapper fallback."
            );
        }
    }

    cargo_cmd.args(&invocation.cargo_args);
    if invocation.has_app_args_separator {
        cargo_cmd.arg("--");
        cargo_cmd.args(&invocation.app_args);
    }

    let status = match cargo_cmd.status() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to spawn cargo: {e}");
            process::exit(1);
        }
    };

    process::exit(status.code().unwrap_or(1));
}

fn supports_native_orchestration(args: &[String]) -> bool {
    matches!(args.first().map(String::as_str), Some("build" | "run"))
        && !args.iter().any(|arg| {
            arg == "--target"
                || arg.starts_with("--target=")
                || arg == "--all-targets"
                || arg == "--tests"
                || arg == "--benches"
        })
}

enum NativeAcquisition {
    Available,
    Tier2Only(String),
}

fn acquire_native_artifacts(
    cargo_args: &[String],
    target_dir: &Path,
    invocation_dir: &Path,
    plan: &mut SessionPlan,
) -> Result<NativeAcquisition, String> {
    let mut prepass_args = without_message_format(cargo_args);
    if prepass_args.first().is_some_and(|arg| arg == "run") {
        // `cargo run` has no `--message-format` flag.  Its build graph is the ordinary
        // `cargo build` graph for the selected executable, so capture artifacts through the
        // supported build command and leave program execution for the final invocation.
        prepass_args[0] = "build".to_string();
    }
    prepass_args.push("--message-format=json-render-diagnostics".into());
    if !prepass_args
        .iter()
        .any(|arg| arg == "--target-dir" || arg.starts_with("--target-dir="))
    {
        prepass_args.push("--target-dir".into());
        prepass_args.push(target_dir.display().to_string());
    }

    let output = Command::new("cargo")
        .current_dir(invocation_dir)
        .args(&prepass_args)
        .env_remove("RUSTC_WRAPPER")
        .env_remove(WRAPPER_MODE_ENV)
        .output()
        .map_err(|e| format!("failed to run same-target artifact pre-pass: {e}"))?;
    let metadata_output = cargo_metadata_output(cargo_args, invocation_dir)?;
    if !metadata_output.status.success() {
        return Err(format!(
            "Cargo metadata failed while preparing recovery: {}",
            String::from_utf8_lossy(&metadata_output.stderr)
        ));
    }
    let metadata: serde_json::Value = serde_json::from_slice(&metadata_output.stdout)
        .map_err(|e| format!("metadata for pre-pass recovery is invalid: {e}"))?;

    let capture_error = if output.status.success() {
        plan.add_r4_artifacts_from_cargo_json(&metadata, &output.stdout, None)
            .err()
            .map(|e| format!("pre-pass artifact output was incomplete: {e}"))
    } else {
        Some(format!(
            "pre-pass exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    };

    let retained_ids = if capture_error.is_none() {
        retained_artifact_closure(plan, &metadata)
    } else {
        std::collections::HashSet::new()
    };
    let instrumented_ids = instrumented_package_ids(plan, &metadata, &retained_ids);
    let freshly_compiled = freshly_compiled_package_ids(&output.stdout);
    let uninstrumented_fresh =
        uninstrumented_fresh_package_ids(&output.stdout, &instrumented_ids, invocation_dir);
    let dirty_instrumented_ids: std::collections::HashSet<String> = match &freshly_compiled {
        Ok(ids) => instrumented_ids
            .intersection(ids)
            .cloned()
            .chain(uninstrumented_fresh)
            .collect(),
        Err(_) => instrumented_ids.clone(),
    };
    if !retained_ids.is_disjoint(&dirty_instrumented_ids) {
        plan.r4_native_otel_artifacts.clear();
        invalidate_packages(
            &dirty_instrumented_ids,
            &retained_ids,
            &metadata,
            cargo_args,
            target_dir,
            invocation_dir,
        )?;
        return Ok(NativeAcquisition::Tier2Only(
            "the OpenTelemetry artifact dependency closure overlaps a unit selected for wrapper instrumentation".into(),
        ));
    }

    let clean_ids: std::collections::HashSet<String> = dirty_instrumented_ids
        .difference(&retained_ids)
        .cloned()
        .collect();
    invalidate_packages(
        &clean_ids,
        &retained_ids,
        &metadata,
        cargo_args,
        target_dir,
        invocation_dir,
    )?;

    if let Some(reason) = capture_error {
        plan.r4_native_otel_artifacts.clear();
        return Ok(NativeAcquisition::Tier2Only(reason));
    }
    if plan.r4_native_otel_artifacts.is_empty() {
        return Ok(NativeAcquisition::Tier2Only(
            "the pre-pass produced no eligible OpenTelemetry artifact".into(),
        ));
    }
    if let Some(missing) = plan
        .r4_native_otel_artifacts
        .iter()
        .find(|artifact| !artifact.rlib_path.is_file())
        .map(|artifact| artifact.rlib_path.clone())
    {
        plan.r4_native_otel_artifacts.clear();
        invalidate_packages(
            &instrumented_ids,
            &retained_ids,
            &metadata,
            cargo_args,
            target_dir,
            invocation_dir,
        )?;
        return Ok(NativeAcquisition::Tier2Only(format!(
            "selective invalidation removed the retained OpenTelemetry artifact '{}'",
            missing.display()
        )));
    }
    Ok(NativeAcquisition::Available)
}

fn freshly_compiled_package_ids(
    cargo_messages: &[u8],
) -> Result<std::collections::HashSet<String>, String> {
    let mut ids = std::collections::HashSet::new();
    for line in cargo_messages
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let message: serde_json::Value = serde_json::from_slice(line)
            .map_err(|error| format!("malformed Cargo JSON artifact message: {error}"))?;
        if message["reason"].as_str() == Some("compiler-artifact")
            && !message["fresh"].as_bool().unwrap_or(false)
        {
            let id = message["package_id"]
                .as_str()
                .ok_or("Cargo compiler-artifact message has no package_id")?;
            ids.insert(id.to_string());
        }
    }
    Ok(ids)
}

fn build_session_plan(cargo_args: &[String], invocation_dir: &Path) -> Result<SessionPlan, String> {
    let output = cargo_metadata_output(cargo_args, invocation_dir)?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("cargo metadata output was malformed: {error}"))?;
    SessionPlan::from_metadata_json(&metadata).map_err(|error| error.to_string())
}

fn cargo_metadata_output(
    cargo_args: &[String],
    invocation_dir: &Path,
) -> Result<std::process::Output, String> {
    let mut metadata_args = vec![
        "metadata".to_string(),
        "--format-version".to_string(),
        "1".to_string(),
    ];
    let mut index = 0;
    while index < cargo_args.len() {
        let arg = &cargo_args[index];
        let takes_value = matches!(
            arg.as_str(),
            "--manifest-path" | "--features" | "--config" | "--filter-platform"
        );
        if takes_value {
            metadata_args.push(arg.clone());
            if let Some(value) = cargo_args.get(index + 1) {
                metadata_args.push(value.clone());
                index += 1;
            }
        } else if arg.starts_with("--manifest-path=")
            || arg.starts_with("--features=")
            || arg.starts_with("--config=")
            || arg.starts_with("--filter-platform=")
            || matches!(
                arg.as_str(),
                "--offline" | "--locked" | "--frozen" | "--all-features" | "--no-default-features"
            )
        {
            metadata_args.push(arg.clone());
        }
        index += 1;
    }
    Command::new("cargo")
        .current_dir(invocation_dir)
        .args(metadata_args)
        .output()
        .map_err(|error| format!("failed to query Cargo metadata: {error}"))
}

fn without_message_format(args: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
        } else if arg == "--message-format" {
            skip_next = true;
        } else if !arg.starts_with("--message-format=") {
            result.push(arg.clone());
        }
    }
    result
}

fn uninstrumented_fresh_package_ids(
    cargo_messages: &[u8],
    instrumented_ids: &std::collections::HashSet<String>,
    invocation_dir: &Path,
) -> std::collections::HashSet<String> {
    let mut uninstrumented = std::collections::HashSet::new();
    for line in cargo_messages
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let Ok(message) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        if message["reason"].as_str() != Some("compiler-artifact") {
            continue;
        }
        let Some(pkg_id) = message["package_id"].as_str() else {
            continue;
        };
        if !instrumented_ids.contains(pkg_id) {
            continue;
        }
        // If not fresh, it was recompiled by the pre-pass, so freshly_compiled already caught it.
        if !message["fresh"].as_bool().unwrap_or(false) {
            continue;
        }
        let Some(target_name) = message["target"]["name"].as_str() else {
            uninstrumented.insert(pkg_id.to_string());
            continue;
        };
        let filenames = message["filenames"].as_array();
        let Some(filenames) = filenames else {
            uninstrumented.insert(pkg_id.to_string());
            continue;
        };
        let target_files: Vec<String> = filenames
            .iter()
            .filter_map(|f| f.as_str().map(String::from))
            .collect();
        if target_files.is_empty() {
            uninstrumented.insert(pkg_id.to_string());
            continue;
        }

        let norm_target = target_name.replace('-', "_");
        let mut artifact_mtimes = Vec::new();
        let mut stamp_files = std::collections::HashSet::new();
        for file_str in &target_files {
            let p = Path::new(file_str);
            let file_path = if p.is_absolute() {
                p.to_path_buf()
            } else {
                invocation_dir.join(p)
            };
            let Ok(file_meta) = std::fs::metadata(&file_path) else {
                continue;
            };
            let Ok(file_mtime) = file_meta.modified() else {
                continue;
            };
            let Some(parent_dir) = file_path.parent() else {
                continue;
            };
            let Some(stamp_name) = artifact_stamp_name(&norm_target, &file_path) else {
                continue;
            };
            let stamp_file = if parent_dir.ends_with("deps") {
                parent_dir.join(stamp_name)
            } else {
                parent_dir.join("deps").join(stamp_name)
            };
            artifact_mtimes.push(file_mtime);
            stamp_files.insert(stamp_file);
        }

        let is_valid_instrumented = !artifact_mtimes.is_empty()
            && !stamp_files.is_empty()
            && stamp_files.iter().all(|stamp_file| {
                std::fs::metadata(stamp_file)
                    .and_then(|metadata| metadata.modified())
                    .map(|stamp_mtime| {
                        artifact_mtimes
                            .iter()
                            .all(|mtime| stamp_mtime + std::time::Duration::from_secs(1) >= *mtime)
                    })
                    .unwrap_or(false)
            });
        if !is_valid_instrumented {
            uninstrumented.insert(pkg_id.to_string());
        }
    }
    uninstrumented
}

/// Recover Cargo's `extra-filename` hash from a hashed artifact filename.
/// Pairing it with the normalized target name uniquely identifies the artifact
/// that Cargo reports as fresh.
fn artifact_stamp_name(normalized_target: &str, artifact: &Path) -> Option<String> {
    let stem = artifact.file_stem()?.to_str()?;
    let direct_prefix = format!("{normalized_target}-");
    let library_prefix = format!("lib{normalized_target}-");
    if let Some(artifact_hash) = stem
        .strip_prefix(&direct_prefix)
        .or_else(|| stem.strip_prefix(&library_prefix))
    {
        UnitId::instrumentation_stamp_name(normalized_target, &format!("-{artifact_hash}"))
    } else if stem == normalized_target || stem == format!("lib{normalized_target}") {
        UnitId::instrumentation_stamp_name(normalized_target, "")
    } else {
        None
    }
}

fn instrumented_package_ids(
    plan: &SessionPlan,
    metadata: &serde_json::Value,
    retained_ids: &std::collections::HashSet<String>,
) -> std::collections::HashSet<String> {
    let registry_enabled = env::var("CARGO_INSTRUMENT_REGISTRY")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|package| {
            let id = package["id"].as_str()?;
            let name = package["name"].as_str()?;
            if !plan.target_reachable_package_ids.contains(id)
                || retained_ids.contains(id)
                || name == "opentelemetry"
                || name.starts_with("opentelemetry_")
                || name == "otel-shim"
                || name == "otel_shim"
                || name == "cargo-instrument"
            {
                return None;
            }
            if package["source"].is_null() || registry_enabled {
                Some(id.to_string())
            } else {
                None
            }
        })
        .collect()
}

fn retained_artifact_closure(
    plan: &SessionPlan,
    metadata: &serde_json::Value,
) -> std::collections::HashSet<String> {
    let mut edges: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for node in metadata["resolve"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if let Some(id) = node["id"].as_str() {
            edges.insert(
                id.to_string(),
                node["deps"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|dep| dep["pkg"].as_str().map(String::from))
                    .collect(),
            );
        }
    }
    let mut retained: std::collections::HashSet<String> = plan
        .r4_native_otel_artifacts
        .iter()
        .map(|artifact| artifact.package_id.clone())
        .collect();
    let mut queue: std::collections::VecDeque<String> = retained.iter().cloned().collect();
    while let Some(id) = queue.pop_front() {
        if let Some(deps) = edges.get(&id) {
            for dep in deps {
                if retained.insert(dep.clone()) {
                    queue.push_back(dep.clone());
                }
            }
        }
    }
    retained
}

fn invalidate_packages(
    package_ids: &std::collections::HashSet<String>,
    retained_ids: &std::collections::HashSet<String>,
    metadata: &serde_json::Value,
    cargo_args: &[String],
    target_dir: &Path,
    invocation_dir: &Path,
) -> Result<(), String> {
    if package_ids.is_empty() {
        return Ok(());
    }

    if env::var("__CARGO_INSTRUMENT_FAULT_INJECT_CLEAN_FAIL").is_ok() {
        return Err("fault injection: cargo clean failed with OS error".into());
    }

    // Build map of all package IDs in workspace metadata by package name
    let mut all_packages_by_name: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    if let Some(packages) = metadata["packages"].as_array() {
        for pkg in packages {
            if let (Some(id), Some(name)) = (pkg["id"].as_str(), pkg["name"].as_str()) {
                all_packages_by_name
                    .entry(name.to_string())
                    .or_default()
                    .push(id.to_string());
            }
        }
    }

    // Retained package names (OpenTelemetry closure)
    let retained_names: std::collections::HashSet<String> = retained_ids
        .iter()
        .filter_map(|id| {
            metadata["packages"]
                .as_array()?
                .iter()
                .find(|p| p["id"].as_str() == Some(id))?["name"]
                .as_str()
                .map(String::from)
        })
        .collect();

    let names: std::collections::HashMap<String, String> = metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|package| {
            Some((
                package["id"].as_str()?.to_string(),
                package["name"].as_str()?.to_string(),
            ))
        })
        .collect();

    let mut packages_by_name: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for id in package_ids {
        let name = names
            .get(id)
            .ok_or_else(|| format!("Cargo metadata has no package name for '{id}'"))?;
        packages_by_name
            .entry(name.clone())
            .or_default()
            .push(id.clone());
    }

    // Safety checks:
    // 1. If any package name to be cleaned matches a package name in the retained OpenTelemetry closure:
    for name in packages_by_name.keys() {
        if retained_names.contains(name) {
            return Err(format!(
                "cannot safely invalidate package '{name}' because it shares a package name with the retained OpenTelemetry artifact closure, and cargo clean would remove retained artifacts"
            ));
        }
    }

    // 2. If there are multiple packages with this name in the workspace dependency graph,
    // verify that ALL of them are in package_ids.
    // Because `cargo clean -p <name>` cleans all versions of <name>, cleaning is unsafe if some versions are NOT to be cleaned.
    for (name, ids_to_clean) in &packages_by_name {
        if let Some(all_ids) = all_packages_by_name.get(name) {
            if all_ids.len() > 1 && ids_to_clean.len() != all_ids.len() {
                return Err(format!(
                    "cannot safely selectively invalidate package '{name}' because multiple versions exist in the dependency graph ({all_ids:?}) and cargo clean does not support version-specific cleaning"
                ));
            }
        }
    }

    // Extract forwarding flags from cargo_args
    let mut forward_flags = Vec::new();
    let mut i = 0;
    while i < cargo_args.len() {
        let arg = &cargo_args[i];
        if matches!(arg.as_str(), "--manifest-path" | "--config") {
            forward_flags.push(arg.clone());
            if let Some(val) = cargo_args.get(i + 1) {
                forward_flags.push(val.clone());
                i += 1;
            }
        } else if arg.starts_with("--manifest-path=")
            || arg.starts_with("--config=")
            || matches!(arg.as_str(), "--offline" | "--locked" | "--frozen")
        {
            forward_flags.push(arg.clone());
        }
        i += 1;
    }

    for package in packages_by_name.keys() {
        let mut clean_cmd = Command::new("cargo");
        clean_cmd
            .current_dir(invocation_dir)
            .arg("clean")
            .arg("--package")
            .arg(package)
            .arg("--target-dir")
            .arg(target_dir)
            .args(&forward_flags);

        let status = clean_cmd
            .status()
            .map_err(|e| format!("failed to invoke cargo clean for package '{package}': {e}"))?;
        if !status.success() {
            return Err(format!(
                "selective invalidation failed for package '{package}': cargo clean exited with {status}"
            ));
        }
        let norm = package.replace('-', "_");
        let prefix = format!(".cargo-instrument-transformed-{norm}");
        for profile in ["debug", "release"] {
            let deps = target_dir.join(profile).join("deps");
            if let Ok(entries) = std::fs::read_dir(&deps) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with(&prefix) || name == format!(".instrumented_{norm}") {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
    }
    Ok(())
}

fn print_help() {
    println!(
        "cargo-instrument {}
Compile-time OpenTelemetry instrumentation wrapper for Rust

USAGE:
    cargo instrument [OPTIONS] -- <cargo-args...>
    cargo instrument analyze <path.rs>
    cargo instrument transform <path.rs> [--output <destination.rs>]

OPTIONS:
    -h, --help       Print help information
    -V, --version    Print version information

SUBCOMMANDS:
    analyze <path.rs>                        Analyze a source file and print discovered candidates
    transform <path.rs> [--output <dest.rs>] Deterministically transform source file using candidate byte ranges

ENVIRONMENT:
    INSTRUMENT_DEBUG     Set to 1 to enable candidate debug output during builds
    RUSTC_WRAPPER        Automatically set by 'cargo instrument' to invoke this binary
",
        env!("CARGO_PKG_VERSION")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_artifact_message(package_id: &str, artifact: &Path) -> Vec<u8> {
        format!(
            "{}\n",
            serde_json::json!({
                "reason": "compiler-artifact",
                "package_id": package_id,
                "fresh": true,
                "target": { "name": "dep" },
                "filenames": [artifact],
            })
        )
        .into_bytes()
    }

    #[test]
    fn fresh_artifact_requires_exact_transformed_unit_marker() {
        let temp = tempfile::tempdir().expect("temp target directory");
        let deps = temp.path().join("debug/deps");
        std::fs::create_dir_all(&deps).expect("create deps directory");
        let artifact = deps.join("libdep-a1b2c3.rlib");
        std::fs::write(&artifact, b"ordinary artifact").expect("write artifact");

        let package_id = "path+file:///fixture#dep@0.1.0";
        let mut instrumented = std::collections::HashSet::new();
        instrumented.insert(package_id.to_string());
        let messages = fresh_artifact_message(package_id, &artifact);

        // The legacy crate-wide marker must never certify this artifact.
        std::fs::write(deps.join(".instrumented_dep"), b"1").expect("write legacy marker");
        assert!(
            uninstrumented_fresh_package_ids(&messages, &instrumented, temp.path())
                .contains(package_id)
        );

        let marker = UnitId::instrumentation_stamp_name("dep", "-a1b2c3")
            .expect("metadata-bearing unit marker");
        std::fs::write(deps.join(marker), b"1").expect("write exact transformed marker");
        assert!(uninstrumented_fresh_package_ids(&messages, &instrumented, temp.path()).is_empty());
    }
}
