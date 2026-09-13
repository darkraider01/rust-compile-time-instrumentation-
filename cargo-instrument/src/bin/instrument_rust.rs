//! `cargo instrument-rust --apply`: first-party, apply-once P2.3 frontend.
//!
//! Cargo owns `RUSTC_WRAPPER` while `cargo fix` is running. This command sets
//! only `RUSTC` to the isolated nightly driver.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde::Serialize;
use serde_json::Value;

const PINNED_P23_TOOLCHAIN: &str = include_str!("../../../tools/p23-toolchain.txt");

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
    let selected_packages = selected_workspace_packages(&metadata, &options.packages)?;
    let toolchain = p23_toolchain();
    let driver = build_driver(options.offline, &toolchain)?;
    let sysroot = nightly_sysroot(&toolchain)?;
    let driver_path = driver_bin_path(&sysroot)?;
    let driver_ld_path = driver_dynamic_library_path(&sysroot, "LD_LIBRARY_PATH")?;
    let driver_dyld_fallback_path =
        driver_dynamic_library_path(&sysroot, "DYLD_FALLBACK_LIBRARY_PATH")?;
    let driver_dyld_path = driver_dynamic_library_path(&sysroot, "DYLD_LIBRARY_PATH")?;

    let mut command = Command::new(cargo_executable());
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
            "CARGO_INSTRUMENT_RUST_SELECTED_PACKAGES",
            serde_json::to_string(&selected_packages)
                .map_err(|error| format!("could not encode selected packages: {error}"))?,
        )
        .env("CARGO_INSTRUMENT_RUST_SYSROOT", &sysroot)
        .env("PATH", driver_path)
        .env("LD_LIBRARY_PATH", driver_ld_path)
        .env("DYLD_FALLBACK_LIBRARY_PATH", driver_dyld_fallback_path)
        .env("DYLD_LIBRARY_PATH", driver_dyld_path);
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
        let first = if first == "instrument-rust" {
            arguments.next().ok_or_else(usage)?
        } else {
            first
        };
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

fn cargo_executable() -> OsString {
    cargo_executable_from(env::var_os("CARGO"))
}

fn cargo_executable_from(invoking_cargo: Option<OsString>) -> OsString {
    invoking_cargo.unwrap_or_else(|| OsString::from("cargo"))
}

fn cargo_metadata(offline: bool) -> Result<Value, String> {
    let mut command = Command::new(cargo_executable());
    command.args(["metadata", "--format-version", "1", "--no-deps"]);
    if offline {
        command.arg("--offline");
    }
    let output = command
        .output()
        .map_err(|error| format!("could not run cargo metadata: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("invalid cargo metadata output: {error}"))
}

#[derive(Serialize)]
struct SelectedPackage {
    name: String,
    source_root: PathBuf,
}

fn selected_workspace_packages(
    metadata: &Value,
    requested: &[String],
) -> Result<Vec<SelectedPackage>, String> {
    let workspace_members = metadata["workspace_members"]
        .as_array()
        .ok_or_else(|| "cargo metadata did not report workspace members".to_string())?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata did not report packages".to_string())?;
    let mut selected_packages = Vec::new();
    for package in packages {
        let id = package["id"].as_str().unwrap_or_default();
        let name = package["name"].as_str().unwrap_or_default();
        let is_workspace_member = workspace_members
            .iter()
            .any(|member| member.as_str() == Some(id));
        let selected = requested.is_empty() || requested.iter().any(|requested| requested == name);
        if selected && is_workspace_member {
            let manifest = package["manifest_path"]
                .as_str()
                .ok_or_else(|| format!("package `{name}` has no manifest path"))?;
            let root = Path::new(manifest)
                .parent()
                .ok_or_else(|| format!("package `{name}` has no manifest parent"))?;
            selected_packages.push(SelectedPackage {
                name: name.to_owned(),
                source_root: root.to_path_buf(),
            });
        }
    }
    if selected_packages.is_empty() {
        return Err("no selected workspace packages matched --package".into());
    }
    for package in requested {
        if !selected_packages
            .iter()
            .any(|selected| &selected.name == package)
        {
            return Err(format!(
                "`{package}` is not a workspace package; P2.3 edits only first-party source"
            ));
        }
    }
    Ok(selected_packages)
}

fn build_driver(offline: bool, toolchain: &str) -> Result<PathBuf, String> {
    if let Some(driver) = env::var_os("CARGO_INSTRUMENT_RUST_DRIVER") {
        return Ok(PathBuf::from(driver));
    }
    let manifest = driver_manifest_path()?;
    let mut command = Command::new(cargo_executable());
    command.args(["build", "--manifest-path"]);
    command.arg(&manifest);
    if offline {
        command.arg("--offline");
    }
    command
        .env("RUSTUP_TOOLCHAIN", toolchain)
        .env_remove("RUSTC")
        .env_remove("RUSTC_WRAPPER");
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

fn nightly_sysroot(toolchain: &str) -> Result<OsString, String> {
    let output = Command::new("rustc")
        .args([format!("+{toolchain}"), "--print".into(), "sysroot".into()])
        .output()
        .map_err(|error| format!("could not locate nightly rustc: {error}"))?;
    if !output.status.success() {
        return Err(format!("rustc +{toolchain} --print sysroot failed"));
    }
    let value = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    Ok(OsString::from(value.trim()))
}

fn p23_toolchain() -> String {
    env::var("CARGO_INSTRUMENT_RUST_TOOLCHAIN")
        .unwrap_or_else(|_| PINNED_P23_TOOLCHAIN.trim().to_owned())
}

fn driver_bin_path(sysroot: &OsString) -> Result<OsString, String> {
    let mut paths = vec![
        PathBuf::from(sysroot).join("bin"),
        PathBuf::from(sysroot).join("lib"),
    ];
    if let Some(existing) = env::var_os("PATH") {
        paths.extend(env::split_paths(&existing));
    }
    env::join_paths(paths).map_err(|error| format!("could not construct driver PATH: {error}"))
}

fn driver_dylib_search_paths(sysroot: &OsString) -> Vec<PathBuf> {
    let sysroot_lib = PathBuf::from(sysroot).join("lib");
    let mut paths = vec![sysroot_lib.clone()];
    if let Ok(entries) = std::fs::read_dir(sysroot_lib.join("rustlib")) {
        for entry in entries.flatten() {
            let target_lib = entry.path().join("lib");
            if target_lib.is_dir() {
                paths.push(target_lib);
            }
        }
    }
    paths
}

fn driver_dynamic_library_path(sysroot: &OsString, var_name: &str) -> Result<OsString, String> {
    let mut paths = driver_dylib_search_paths(sysroot);
    if let Some(existing) = env::var_os(var_name) {
        paths.extend(env::split_paths(&existing));
    }
    env::join_paths(paths)
        .map_err(|error| format!("could not construct driver {var_name}: {error}"))
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

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{
        cargo_executable_from, driver_dylib_search_paths, driver_dynamic_library_path, Options,
    };

    #[test]
    fn accepts_direct_binary_invocation() {
        assert!(Options::parse(["--apply".into()].into_iter()).is_ok());
    }

    #[test]
    fn accepts_cargo_forwarded_subcommand_name() {
        let options = Options::parse(
            ["instrument-rust", "--apply", "--offline"]
                .map(str::to_owned)
                .into_iter(),
        )
        .unwrap();
        assert!(options.offline);
    }

    #[test]
    fn prefers_cargos_invoking_executable() {
        assert_eq!(
            cargo_executable_from(Some(OsString::from("custom-cargo"))),
            OsString::from("custom-cargo")
        );
        assert_eq!(cargo_executable_from(None), OsString::from("cargo"));
    }

    #[test]
    fn driver_dylib_search_paths_includes_sysroot_lib_and_rustlib_targets() {
        let temp = tempfile::tempdir().unwrap();
        let sysroot = temp.path();
        let lib = sysroot.join("lib");
        let target_lib = lib.join("rustlib/x86_64-unknown-linux-gnu/lib");
        std::fs::create_dir_all(&target_lib).unwrap();

        let paths = driver_dylib_search_paths(&sysroot.as_os_str().to_os_string());
        assert!(paths.contains(&lib));
        assert!(paths.contains(&target_lib));
    }

    #[test]
    fn driver_dynamic_library_path_prepends_to_existing_env() {
        let temp = tempfile::tempdir().unwrap();
        let sysroot = temp.path();
        let lib = sysroot.join("lib");
        std::fs::create_dir_all(&lib).unwrap();

        let joined = driver_dynamic_library_path(
            &sysroot.as_os_str().to_os_string(),
            "NONEXISTENT_TEST_DYLIB_VAR",
        )
        .unwrap();
        assert!(joined
            .to_string_lossy()
            .contains(&lib.to_string_lossy().to_string()));
    }
}
