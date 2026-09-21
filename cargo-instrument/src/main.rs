use std::env;
use std::path::{Path, PathBuf};
use std::process::{self, Command};

use cargo_instrument::{
    analyze_source_file, run_wrapper, transform_source_file, transform_source_str, SessionPlan,
    WrapperConfig, DEBUG_ENV, SESSION_ENV,
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

    // Check if user already provided --target-dir
    let has_target_dir = args
        .iter()
        .any(|a| a == "--target-dir" || a.starts_with("--target-dir="));

    // Determine arguments to pass to Cargo
    // Separate flags before `--` from cargo command
    let mut cargo_args: Vec<String> = Vec::new();

    for arg in args {
        if arg == "--" {
            continue;
        }
        cargo_args.push(arg.clone());
    }

    // If double dash was used with no cargo subcommand (e.g. `cargo instrument -- build`), cargo_args has ["build"]
    // If no cargo subcommand was specified at all, default to "build"
    if cargo_args.is_empty() {
        cargo_args.push("build".to_string());
    }

    // Append isolated --target-dir target/instrumented (ADR-004) if not already provided
    if !has_target_dir {
        cargo_args.push("--target-dir".to_string());
        cargo_args.push(DEFAULT_TARGET_DIR.to_string());
    }

    let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // Extract target directory from args or use DEFAULT_TARGET_DIR
    let target_dir_str = args
        .iter()
        .position(|a| a == "--target-dir")
        .and_then(|idx| args.get(idx + 1).cloned())
        .or_else(|| {
            args.iter()
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
    match build_session_plan(&cargo_args, &current_dir) {
        Ok(mut plan) => {
            // Native R-4 acquisition is deliberately limited to ordinary build/run.  The
            // pre-pass uses this exact target directory so Cargo's crate identities remain
            // compatible with the final wrapper-enabled build.  Unsupported commands retain
            // the established wrapper-only behavior.
            if supports_native_orchestration(&cargo_args) {
                match acquire_native_artifacts(
                    &cargo_args,
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

    cargo_cmd.args(&cargo_args);

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

    let freshly_compiled = freshly_compiled_package_ids(&output.stdout);
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

    let instrumented_ids = instrumented_package_ids(plan, &metadata);
    // Cargo only poisons freshness for units that this pre-pass actually rebuilt. Fresh
    // artifacts in the isolated target directory are prior wrapper output. If JSON is not
    // trustworthy, invalidate the whole selected set rather than relying on that distinction.
    let dirty_instrumented_ids: std::collections::HashSet<String> = match &freshly_compiled {
        Ok(ids) => instrumented_ids.intersection(ids).cloned().collect(),
        Err(_) => instrumented_ids.clone(),
    };
    let retained_ids = if capture_error.is_none() {
        retained_artifact_closure(plan, &metadata)
    } else {
        std::collections::HashSet::new()
    };
    if !retained_ids.is_disjoint(&dirty_instrumented_ids) {
        plan.r4_native_otel_artifacts.clear();
        invalidate_packages(
            &dirty_instrumented_ids,
            &metadata,
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
    invalidate_packages(&clean_ids, &metadata, target_dir, invocation_dir)?;

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
        invalidate_packages(&instrumented_ids, &metadata, target_dir, invocation_dir)?;
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

fn instrumented_package_ids(
    plan: &SessionPlan,
    metadata: &serde_json::Value,
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
    metadata: &serde_json::Value,
    target_dir: &Path,
    invocation_dir: &Path,
) -> Result<(), String> {
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
    if let Some((name, ids)) = packages_by_name.iter().find(|(_, ids)| ids.len() > 1) {
        return Err(format!(
            "cannot safely selectively invalidate multiple selected packages named '{name}' because this Cargo version does not honor source-qualified clean package IDs: {ids:?}"
        ));
    }
    for (package, _) in packages_by_name {
        let status = Command::new("cargo")
            .current_dir(invocation_dir)
            .args([
                "clean",
                "--package",
                &package,
                "--target-dir",
                &target_dir.display().to_string(),
            ])
            .status()
            .map_err(|e| format!("failed to selectively invalidate {package}: {e}"))?;
        if !status.success() {
            return Err(format!(
                "selective invalidation failed for package {package}"
            ));
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
