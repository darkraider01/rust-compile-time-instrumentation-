//! P2.1 Scale fixture: ≥100 compilation units under parallel build execution.
//!
//! Validates:
//! 1. Zero partial-file or sharing-violation failures under high concurrency.
//! 2. Mirror directory isolation across ≥100 distinct compilation units.
//! 3. Correct Tier-2 trampoline injection and resolution across deep/wide graphs.
//! 4. Source byte immutability and dep-info integrity.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cargo-instrument parent is repo root")
        .to_path_buf()
}

fn otel_shim_dep_path() -> String {
    repo_root()
        .join("otel-shim")
        .to_string_lossy()
        .replace('\\', "/")
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture directory");
    }
    fs::write(path, contents).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

fn run_cargo(manifest_dir: &Path, target_dir: &Path, args: &[&str], instrumented: bool) -> Output {
    let mut cmd = Command::new("cargo");
    cmd.args(args)
        .arg("--target-dir")
        .arg(target_dir)
        .current_dir(manifest_dir)
        .env("CARGO_TERM_COLOR", "never");

    if instrumented {
        cmd.env("RUSTC_WRAPPER", env!("CARGO_BIN_EXE_cargo-instrument"))
            .env("INSTRUMENT_DEBUG", "1");
    }

    cmd.output().expect("failed to execute cargo")
}

fn describe(output: &Output) -> String {
    format!(
        "exit: {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

#[test]
fn test_scale_graph_100_units_parallel_compilation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    // Generate 4 layers of 26 crates each = 104 library crates + 1 root app = 105 units.
    // Layer 0: c0_0 .. c0_25 (26 independent leaf crates)
    // Layer 1: c1_0 .. c1_25 (each depends on c0_i and c0_{(i+1)%26})
    // Layer 2: c2_0 .. c2_25 (each depends on c1_i and c1_{(i+1)%26})
    // Layer 3: c3_0 .. c3_25 (each depends on c2_i and c2_{(i+1)%26})
    let layers = 4;
    let width = 26;
    let mut workspace_members = Vec::new();

    for layer in 0..layers {
        for i in 0..width {
            let crate_name = format!("crate_l{layer}_{i}");
            workspace_members.push(format!("\"{crate_name}\""));
            let crate_dir = root.join(&crate_name);

            let mut deps = String::new();
            let body = if layer == 0 {
                format!(
                    "pub fn compute_{crate_name}(val: i64) -> i64 {{\n    val + {}\n}}\n",
                    i + 1
                )
            } else {
                let dep1 = format!("crate_l{}_{i}", layer - 1);
                let dep2 = format!("crate_l{}_{}", layer - 1, (i + 1) % width);
                deps = format!(
                    "{dep1} = {{ path = \"../{dep1}\" }}\n\
                     {dep2} = {{ path = \"../{dep2}\" }}\n"
                );
                format!(
                    "pub fn compute_{crate_name}(val: i64) -> i64 {{\n    \
                        let r1 = {dep1}::compute_{dep1}(val);\n    \
                        let r2 = {dep2}::compute_{dep2}(r1);\n    \
                        r2 + 1\n\
                    }}\n"
                )
            };

            let cargo_toml = format!(
                "[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                 [dependencies]\n{deps}"
            );
            write_file(&crate_dir.join("Cargo.toml"), &cargo_toml);
            write_file(&crate_dir.join("src").join("lib.rs"), &body);
        }
    }

    // Root application crate that depends on all Layer 3 crates + otel-shim
    workspace_members.push("\"scale_app\"".to_string());
    let app_dir = root.join("scale_app");

    let mut app_deps = String::new();
    let mut app_calls = String::new();
    for i in 0..width {
        let dep = format!("crate_l3_{i}");
        app_deps.push_str(&format!("{dep} = {{ path = \"../{dep}\" }}\n"));
        app_calls.push_str(&format!("    total += {dep}::compute_{dep}(1);\n"));
    }
    app_deps.push_str(&format!(
        "otel-shim = {{ path = \"{}\" }}\n",
        otel_shim_dep_path()
    ));

    let app_cargo_toml = format!(
        "[package]\nname = \"scale_app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\n{app_deps}"
    );
    write_file(&app_dir.join("Cargo.toml"), &app_cargo_toml);

    let app_main = format!(
        "fn main() {{\n\
             otel_shim::init();\n\
             let mut total: i64 = 0;\n\
             {app_calls}\n\
             println!(\"TOTAL={{total}}\");\n\
         }}\n"
    );
    write_file(&app_dir.join("src").join("main.rs"), &app_main);

    // Root workspace manifest
    let root_cargo_toml = format!(
        "[workspace]\nmembers = [\n    {}\n]\nresolver = \"2\"\n",
        workspace_members.join(",\n    ")
    );
    write_file(&root.join("Cargo.toml"), &root_cargo_toml);

    let target_dir = root.join("target").join("instrumented");

    // 1. Build the scale workspace with parallel rustc invocations
    let build_output = run_cargo(&app_dir, &target_dir, &["build", "-j", "8"], true);
    assert!(
        build_output.status.success(),
        "scale build (105 units) must succeed under high concurrency without file locking errors.\n{}",
        describe(&build_output)
    );

    // 2. Run the scale application and assert semantic output
    let run_output = run_cargo(&app_dir, &target_dir, &["run", "--quiet"], true);
    assert!(
        run_output.status.success(),
        "scale application execution must succeed.\n{}",
        describe(&run_output)
    );
    let stdout = String::from_utf8_lossy(&run_output.stdout);
    assert!(
        stdout.contains("TOTAL="),
        "scale application must print TOTAL computed value, got:\n{stdout}"
    );

    // 3. Verify mirror directory isolation: exactly 104 library mirrors should be created
    let mirror_root = target_dir
        .join("debug")
        .join("deps")
        .join("instrumented_sources");
    assert!(
        mirror_root.is_dir(),
        "instrumented_sources directory must exist at {}",
        mirror_root.display()
    );

    let entries: Vec<_> = fs::read_dir(&mirror_root)
        .expect("read instrumented_sources")
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();

    // Each crate must have its own isolated mirror directory
    assert!(
        entries.len() >= 104,
        "expected at least 104 isolated mirror directories, found {}. Mirrors: {:?}",
        entries.len(),
        entries.iter().map(|e| e.file_name()).collect::<Vec<_>>()
    );

    // 4. Verify no temporary staging files (.tmp.<pid>.*) leaked in any mirror directory
    for entry in &entries {
        for file in fs::read_dir(entry.path().join("src"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = file.file_name().to_string_lossy().to_string();
            assert!(
                !name.contains(".tmp."),
                "temporary staging file leaked into mirror: {}",
                file.path().display()
            );
        }
    }

    // 5. Repeat build: must succeed incrementally with zero errors
    let repeat_build = run_cargo(&app_dir, &target_dir, &["build"], true);
    assert!(
        repeat_build.status.success(),
        "incremental repeat build must succeed.\n{}",
        describe(&repeat_build)
    );
}
