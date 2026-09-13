//! `cargo instrument-rust --apply`: first-party, apply-once P2.3 frontend.
//!
//! Cargo owns `RUSTC_WRAPPER` while `cargo fix` is running. This command sets
//! only `RUSTC` to the isolated nightly driver.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde_json::Value;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cargo-instrument-rust: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let options = Options::parse(env::args().skip(1))?;
    require_clean_git_tree()?;

    let metadata = cargo_metadata(options.offline)?;
    let roots = selected_source_roots(&metadata, &options.packages)?;
    let package_names = selected_workspace_package_names(&metadata, &options.packages)?;
    let driver = build_driver(options.offline)?;
    let sysroot = nightly_sysroot()?;
    let driver_path = driver_library_path(&sysroot)?;

    let mut command = Command::new("cargo");
    // Cargo checks for VCS below each package root. A virtual workspace may
    // keep its repository one level above those roots; our explicit clean-tree
    // gate above is the authoritative safety check for this command.
    command.args(["fix", "--allow-no-vcs"]);
    if options.packages.is_empty() {
        command.arg("--workspace");
    } else {
        for package in &options.packages {
            command.args(["--package", package]);
        }
    }
    if options.offline {
        command.arg("--offline");
    }
    command
        .env("RUSTC", driver)
        .env(
            "CARGO_INSTRUMENT_RUST_ROOTS",
            env::join_paths(roots).map_err(|error| error.to_string())?,
        )
        .env("CARGO_INSTRUMENT_RUST_PACKAGES", package_names.join(";"))
        .env("CARGO_INSTRUMENT_RUST_SYSROOT", sysroot)
        .env("PATH", driver_path);
    // Deliberately leave RUSTC_WRAPPER alone: Cargo installs its rustfix proxy.
    run_status(&mut command, "cargo fix")
}

#[derive(Default)]
struct Options {
    packages: Vec<String>,
    offline: bool,
}

impl Options {
    fn parse(arguments: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut options = Self::default();
        let mut arguments = arguments.peekable();
        let Some(first) = arguments.next() else {
            return Err(usage());
        };
        if first == "--help" || first == "-h" {
            println!("{}", usage());
            std::process::exit(0);
        }
        if first != "--apply" {
            return Err(usage());
        }
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--package" | "-p" => options.packages.push(
                    arguments
                        .next()
                        .ok_or_else(|| "--package requires a package name".to_string())?,
                ),
                "--offline" => options.offline = true,
                "--help" | "-h" => return Err(usage()),
                _ => return Err(format!("unsupported argument `{argument}`\n{}", usage())),
            }
        }
        Ok(options)
    }
}

fn usage() -> String {
    "usage: cargo instrument-rust --apply [--package <workspace-package>] [--offline]".into()
}

fn require_clean_git_tree() -> Result<(), String> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .map_err(|error| format!("--apply requires a Git worktree: {error}"))?;
    if !output.status.success() {
        return Err("--apply requires a Git worktree".into());
    }
    if !output.stdout.is_empty() {
        return Err("--apply refuses a dirty Git worktree; commit or stash changes first".into());
    }
    Ok(())
}

fn cargo_metadata(offline: bool) -> Result<Value, String> {
    let mut command = Command::new("cargo");
    command.args(["metadata", "--format-version", "1", "--no-deps"]);
    if offline {
        command.arg("--offline");
    }
    let output = command
        .output()
        .map_err(|error| format!("could not run cargo metadata: {error}"))?;
    if !output.status.success() {
        return Err(format!("cargo metadata failed with {}", output.status));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("invalid cargo metadata output: {error}"))
}

fn selected_source_roots(metadata: &Value, requested: &[String]) -> Result<Vec<PathBuf>, String> {
    let workspace_members = metadata["workspace_members"]
        .as_array()
        .ok_or_else(|| "cargo metadata did not report workspace members".to_string())?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata did not report packages".to_string())?;
    let mut roots = Vec::new();
    for package in packages {
        let id = package["id"].as_str().unwrap_or_default();
        let name = package["name"].as_str().unwrap_or_default();
        let is_workspace_member = workspace_members
            .iter()
            .any(|member| member.as_str() == Some(id));
        let selected = requested.is_empty() || requested.iter().any(|requested| requested == name);
        if selected && !is_workspace_member {
            return Err(format!(
                "`{name}` is not a workspace package; P2.3 edits only first-party source"
            ));
        }
        if selected && is_workspace_member {
            let manifest = package["manifest_path"]
                .as_str()
                .ok_or_else(|| format!("package `{name}` has no manifest path"))?;
            let root = Path::new(manifest)
                .parent()
                .ok_or_else(|| format!("package `{name}` has no manifest parent"))?;
            roots.push(root.to_path_buf());
        }
    }
    if roots.is_empty() {
        return Err("no selected workspace packages matched --package".into());
    }
    Ok(roots)
}

fn selected_workspace_package_names(
    metadata: &Value,
    requested: &[String],
) -> Result<Vec<String>, String> {
    let workspace_members = metadata["workspace_members"]
        .as_array()
        .ok_or_else(|| "cargo metadata did not report workspace members".to_string())?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata did not report packages".to_string())?;
    let names: Vec<String> = packages
        .iter()
        .filter(|package| {
            let id = package["id"].as_str().unwrap_or_default();
            let name = package["name"].as_str().unwrap_or_default();
            workspace_members
                .iter()
                .any(|member| member.as_str() == Some(id))
                && (requested.is_empty() || requested.iter().any(|requested| requested == name))
        })
        .filter_map(|package| package["name"].as_str().map(str::to_owned))
        .collect();
    if names.is_empty() {
        return Err("no selected workspace packages matched --package".into());
    }
    Ok(names)
}

fn build_driver(offline: bool) -> Result<PathBuf, String> {
    if let Some(driver) = env::var_os("CARGO_INSTRUMENT_RUST_DRIVER") {
        return Ok(PathBuf::from(driver));
    }
    let manifest = driver_manifest_path()?;
    let mut command = Command::new("cargo");
    command.args(["+nightly", "build", "--manifest-path"]);
    command.arg(&manifest);
    if offline {
        command.arg("--offline");
    }
    command.env_remove("RUSTC").env_remove("RUSTC_WRAPPER");
    run_status(&mut command, "nightly driver build")?;
    let executable = if cfg!(windows) {
        "cargo-instrument-rust-driver.exe"
    } else {
        "cargo-instrument-rust-driver"
    };
    let driver = manifest
        .parent()
        .ok_or_else(|| "driver manifest has no parent".to_string())?
        .join("target")
        .join("debug")
        .join(executable);
    if !driver.is_file() {
        return Err(format!(
            "nightly driver was not produced at {}",
            driver.display()
        ));
    }
    Ok(driver)
}

fn nightly_sysroot() -> Result<OsString, String> {
    let output = Command::new("rustc")
        .args(["+nightly", "--print", "sysroot"])
        .output()
        .map_err(|error| format!("could not locate nightly rustc: {error}"))?;
    if !output.status.success() {
        return Err("rustc +nightly --print sysroot failed".into());
    }
    let value = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    Ok(OsString::from(value.trim()))
}

fn driver_library_path(sysroot: &OsString) -> Result<OsString, String> {
    let mut paths = vec![PathBuf::from(sysroot).join("bin")];
    if let Some(existing) = env::var_os("PATH") {
        paths.extend(env::split_paths(&existing));
    }
    env::join_paths(paths).map_err(|error| format!("could not construct driver PATH: {error}"))
}

fn driver_manifest_path() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| "cargo-instrument manifest has no workspace parent".into())
        .map(|root| root.join("tools/cargo-instrument-rust-driver/Cargo.toml"))
}

fn run_status(command: &mut Command, description: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("could not run {description}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{description} failed with {status}"))
    }
}
