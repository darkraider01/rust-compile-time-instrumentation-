use std::env;
use std::path::{Path, PathBuf};
use std::process::{self, Command};

use cargo_instrument::{
    analyze_source_file, run_wrapper, transform_source_file, transform_source_str, WrapperConfig,
    DEBUG_ENV,
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
