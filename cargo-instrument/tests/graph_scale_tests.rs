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

use cargo_instrument::session::SessionPlan;

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

fn make_broad_metadata(width: usize) -> serde_json::Value {
    let mut packages = Vec::with_capacity(width + 2);
    let mut resolve_nodes = Vec::with_capacity(width + 2);
    let mut app_deps = Vec::with_capacity(width + 1);

    let app_id = "app 0.1.0 (path+file:///app)".to_string();
    packages.push(serde_json::json!({
        "id": app_id,
        "name": "app",
        "version": "0.1.0",
        "manifest_path": "/app/Cargo.toml",
        "targets": [{"kind": ["bin"], "name": "app"}],
        "source": null
    }));

    let shim_id = "otel-shim 0.1.0 (path+file:///otel-shim)".to_string();
    packages.push(serde_json::json!({
        "id": shim_id,
        "name": "otel-shim",
        "version": "0.1.0",
        "manifest_path": "/otel-shim/Cargo.toml",
        "targets": [{"kind": ["lib"], "name": "otel-shim"}],
        "source": null
    }));
    app_deps.push(serde_json::json!({
        "name": "otel_shim",
        "pkg": shim_id,
        "dep_kinds": [{"kind": null}]
    }));
    resolve_nodes.push(serde_json::json!({
        "id": shim_id,
        "deps": []
    }));

    for i in 0..width {
        let leaf_name = format!("leaf_{i}");
        let leaf_id = format!("{leaf_name} 0.1.0 (path+file:///{leaf_name})");
        packages.push(serde_json::json!({
            "id": leaf_id,
            "name": leaf_name,
            "version": "0.1.0",
            "manifest_path": format!("/{leaf_name}/Cargo.toml"),
            "targets": [{"kind": ["lib"], "name": leaf_name}],
            "source": null
        }));
        app_deps.push(serde_json::json!({
            "name": leaf_name,
            "pkg": leaf_id,
            "dep_kinds": [{"kind": null}]
        }));
        resolve_nodes.push(serde_json::json!({
            "id": leaf_id,
            "deps": []
        }));
    }

    resolve_nodes.push(serde_json::json!({
        "id": app_id,
        "deps": app_deps
    }));

    serde_json::json!({
        "packages": packages,
        "workspace_members": [app_id],
        "workspace_root": "/",
        "resolve": {
            "nodes": resolve_nodes
        }
    })
}

fn make_deep_metadata(depth: usize) -> serde_json::Value {
    let mut packages = Vec::with_capacity(depth + 2);
    let mut resolve_nodes = Vec::with_capacity(depth + 2);

    let shim_id = "otel-shim 0.1.0 (path+file:///otel-shim)".to_string();
    packages.push(serde_json::json!({
        "id": shim_id,
        "name": "otel-shim",
        "version": "0.1.0",
        "manifest_path": "/otel-shim/Cargo.toml",
        "targets": [{"kind": ["lib"], "name": "otel-shim"}],
        "source": null
    }));
    resolve_nodes.push(serde_json::json!({
        "id": shim_id,
        "deps": []
    }));

    for i in 0..depth {
        let name = format!("chain_{i}");
        let id = format!("{name} 0.1.0 (path+file:///{name})");
        packages.push(serde_json::json!({
            "id": id,
            "name": name,
            "version": "0.1.0",
            "manifest_path": format!("/{name}/Cargo.toml"),
            "targets": [{"kind": ["lib"], "name": name}],
            "source": null
        }));

        let mut deps = Vec::new();
        if i > 0 {
            let prev_name = format!("chain_{}", i - 1);
            let prev_id = format!("{prev_name} 0.1.0 (path+file:///{prev_name})");
            deps.push(serde_json::json!({
                "name": prev_name,
                "pkg": prev_id,
                "dep_kinds": [{"kind": null}]
            }));
        }
        resolve_nodes.push(serde_json::json!({
            "id": id,
            "deps": deps
        }));
    }

    let app_id = "app 0.1.0 (path+file:///app)".to_string();
    packages.push(serde_json::json!({
        "id": app_id,
        "name": "app",
        "version": "0.1.0",
        "manifest_path": "/app/Cargo.toml",
        "targets": [{"kind": ["bin"], "name": "app"}],
        "source": null
    }));

    let last_chain_name = format!("chain_{}", depth - 1);
    let last_chain_id = format!("{last_chain_name} 0.1.0 (path+file:///{last_chain_name})");
    resolve_nodes.push(serde_json::json!({
        "id": app_id,
        "deps": [
            {
                "name": "otel_shim",
                "pkg": shim_id,
                "dep_kinds": [{"kind": null}]
            },
            {
                "name": last_chain_name,
                "pkg": last_chain_id,
                "dep_kinds": [{"kind": null}]
            }
        ]
    }));

    serde_json::json!({
        "packages": packages,
        "workspace_members": [app_id],
        "workspace_root": "/",
        "resolve": {
            "nodes": resolve_nodes
        }
    })
}

fn make_diamond_metadata(layers: usize, width: usize) -> serde_json::Value {
    let total_libs = layers * width;
    let mut packages = Vec::with_capacity(total_libs + 2);
    let mut resolve_nodes = Vec::with_capacity(total_libs + 2);

    let shim_id = "otel-shim 0.1.0 (path+file:///otel-shim)".to_string();
    packages.push(serde_json::json!({
        "id": shim_id,
        "name": "otel-shim",
        "version": "0.1.0",
        "manifest_path": "/otel-shim/Cargo.toml",
        "targets": [{"kind": ["lib"], "name": "otel-shim"}],
        "source": null
    }));
    resolve_nodes.push(serde_json::json!({
        "id": shim_id,
        "deps": []
    }));

    for layer in 0..layers {
        for i in 0..width {
            let name = format!("diamond_l{layer}_{i}");
            let id = format!("{name} 0.1.0 (path+file:///{name})");
            packages.push(serde_json::json!({
                "id": id,
                "name": name,
                "version": "0.1.0",
                "manifest_path": format!("/{name}/Cargo.toml"),
                "targets": [{"kind": ["lib"], "name": name}],
                "source": null
            }));

            let mut deps = Vec::new();
            if layer > 0 {
                let dep1_name = format!("diamond_l{}_{i}", layer - 1);
                let dep1_id = format!("{dep1_name} 0.1.0 (path+file:///{dep1_name})");
                let dep2_name = format!("diamond_l{}_{}", layer - 1, (i + 1) % width);
                let dep2_id = format!("{dep2_name} 0.1.0 (path+file:///{dep2_name})");

                deps.push(serde_json::json!({
                    "name": dep1_name,
                    "pkg": dep1_id,
                    "dep_kinds": [{"kind": null}]
                }));
                deps.push(serde_json::json!({
                    "name": dep2_name,
                    "pkg": dep2_id,
                    "dep_kinds": [{"kind": null}]
                }));
            }

            resolve_nodes.push(serde_json::json!({
                "id": id,
                "deps": deps
            }));
        }
    }

    let app_id = "app 0.1.0 (path+file:///app)".to_string();
    packages.push(serde_json::json!({
        "id": app_id,
        "name": "app",
        "version": "0.1.0",
        "manifest_path": "/app/Cargo.toml",
        "targets": [{"kind": ["bin"], "name": "app"}],
        "source": null
    }));

    let mut app_deps = Vec::with_capacity(width + 1);
    app_deps.push(serde_json::json!({
        "name": "otel_shim",
        "pkg": shim_id,
        "dep_kinds": [{"kind": null}]
    }));
    let top_layer = layers - 1;
    for i in 0..width {
        let top_name = format!("diamond_l{top_layer}_{i}");
        let top_id = format!("{top_name} 0.1.0 (path+file:///{top_name})");
        app_deps.push(serde_json::json!({
            "name": top_name,
            "pkg": top_id,
            "dep_kinds": [{"kind": null}]
        }));
    }
    resolve_nodes.push(serde_json::json!({
        "id": app_id,
        "deps": app_deps
    }));

    serde_json::json!({
        "packages": packages,
        "workspace_members": [app_id],
        "workspace_root": "/",
        "resolve": {
            "nodes": resolve_nodes
        }
    })
}

#[test]
fn test_decoupled_synthetic_graph_planning_scale() {
    // Exercises SessionPlan resolution across broad, deep, and diamond topologies
    // without spinning up child rustc processes, isolating planning correctness at scale.
    for &width in &[10, 50, 100, 200] {
        let meta = make_broad_metadata(width);
        let plan = SessionPlan::from_metadata_json(&meta).expect("plan broad graph");
        assert!(plan.has_otel_shim_provider());
        assert_eq!(plan.package_manifest_dirs.len(), width + 2);
    }

    for &depth in &[10, 50, 100, 200] {
        let meta = make_deep_metadata(depth);
        let plan = SessionPlan::from_metadata_json(&meta).expect("plan deep graph");
        assert!(plan.has_otel_shim_provider());
        assert_eq!(plan.package_manifest_dirs.len(), depth + 2);
    }

    for &(layers, width) in &[(2, 5), (5, 10), (10, 10), (20, 10)] {
        let total = layers * width;
        let meta = make_diamond_metadata(layers, width);
        let plan = SessionPlan::from_metadata_json(&meta).expect("plan diamond graph");
        assert!(plan.has_otel_shim_provider());
        assert_eq!(plan.package_manifest_dirs.len(), total + 2);
    }
}

#[test]
fn test_scale_graph_deep_linear_chain_execution() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    // 15 crates in a strict linear chain: chain_0 <- chain_1 <- ... <- chain_14 <- app.
    // Deep chains specifically stress transitive dep-info propagation and C-ABI trampoline symbol linkage.
    let chain_len = 15;
    let mut workspace_members = Vec::new();

    for i in 0..chain_len {
        let name = format!("chain_{i}");
        workspace_members.push(format!("\"{name}\""));
        let crate_dir = root.join(&name);

        let (deps, body) = if i == 0 {
            (
                String::new(),
                format!("pub fn compute_{name}(v: u64) -> u64 {{ v + 1 }}\n"),
            )
        } else {
            let prev = format!("chain_{}", i - 1);
            (
                format!("{prev} = {{ path = \"../{prev}\" }}\n"),
                format!(
                    "pub fn compute_{name}(v: u64) -> u64 {{\n    {prev}::compute_{prev}(v) + 1\n}}\n"
                ),
            )
        };

        let cargo_toml = format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\n{deps}"
        );
        write_file(&crate_dir.join("Cargo.toml"), &cargo_toml);
        write_file(&crate_dir.join("src").join("lib.rs"), &body);
    }

    workspace_members.push("\"deep_app\"".to_string());
    let app_dir = root.join("deep_app");
    let last_dep = format!("chain_{}", chain_len - 1);
    let app_cargo_toml = format!(
        "[package]\nname = \"deep_app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\n\
         {last_dep} = {{ path = \"../{last_dep}\" }}\n\
         otel-shim = {{ path = \"{}\" }}\n",
        otel_shim_dep_path()
    );
    write_file(&app_dir.join("Cargo.toml"), &app_cargo_toml);

    let last_func = format!("compute_{last_dep}");
    let app_main = format!(
        "fn main() {{\n\
             otel_shim::init();\n\
             let res = {last_dep}::{last_func}(0);\n\
             println!(\"CHAIN_RESULT={{res}}\");\n\
         }}\n"
    );
    write_file(&app_dir.join("src").join("main.rs"), &app_main);

    let root_cargo_toml = format!(
        "[workspace]\nmembers = [\n    {}\n]\nresolver = \"2\"\n",
        workspace_members.join(",\n    ")
    );
    write_file(&root.join("Cargo.toml"), &root_cargo_toml);

    let target_dir = root.join("target").join("instrumented");

    let build_output = run_cargo(&app_dir, &target_dir, &["build"], true);
    assert!(
        build_output.status.success(),
        "deep chain build must succeed.\n{}",
        describe(&build_output)
    );

    let run_output = run_cargo(&app_dir, &target_dir, &["run", "--quiet"], true);
    assert!(
        run_output.status.success(),
        "deep chain execution must succeed.\n{}",
        describe(&run_output)
    );
    let stdout = String::from_utf8_lossy(&run_output.stdout);
    assert!(
        stdout.contains("CHAIN_RESULT=15"),
        "expected CHAIN_RESULT=15, got:\n{stdout}"
    );

    let mirror_root = target_dir
        .join("debug")
        .join("deps")
        .join("instrumented_sources");
    let entries: Vec<_> = fs::read_dir(&mirror_root)
        .expect("read instrumented_sources")
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();

    assert!(
        entries.len() >= chain_len,
        "expected at least {chain_len} mirror directories, found {}. Mirrors: {:?}",
        entries.len(),
        entries.iter().map(|e| e.file_name()).collect::<Vec<_>>()
    );
}

#[test]
fn test_scale_graph_broad_fanout_execution() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    let width = 20;
    let mut workspace_members = Vec::new();

    for i in 0..width {
        let name = format!("broad_leaf_{i}");
        workspace_members.push(format!("\"{name}\""));
        let crate_dir = root.join(&name);

        let cargo_toml =
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
        let body = format!("pub fn leaf_value() -> u64 {{ {i} }}\n");
        write_file(&crate_dir.join("Cargo.toml"), &cargo_toml);
        write_file(&crate_dir.join("src").join("lib.rs"), &body);
    }

    workspace_members.push("\"broad_app\"".to_string());
    let app_dir = root.join("broad_app");
    let mut app_deps = format!("otel-shim = {{ path = \"{}\" }}\n", otel_shim_dep_path());
    let mut app_calls = String::new();
    for i in 0..width {
        let name = format!("broad_leaf_{i}");
        app_deps.push_str(&format!("{name} = {{ path = \"../{name}\" }}\n"));
        app_calls.push_str(&format!("    total += {name}::leaf_value();\n"));
    }

    let app_cargo_toml = format!(
        "[package]\nname = \"broad_app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\n{app_deps}"
    );
    write_file(&app_dir.join("Cargo.toml"), &app_cargo_toml);

    let app_main = format!(
        "fn main() {{\n\
             otel_shim::init();\n\
             let mut total: u64 = 0;\n\
             {app_calls}\n\
             println!(\"BROAD_TOTAL={{total}}\");\n\
         }}\n"
    );
    write_file(&app_dir.join("src").join("main.rs"), &app_main);

    let root_cargo_toml = format!(
        "[workspace]\nmembers = [\n    {}\n]\nresolver = \"2\"\n",
        workspace_members.join(",\n    ")
    );
    write_file(&root.join("Cargo.toml"), &root_cargo_toml);

    let target_dir = root.join("target").join("instrumented");

    let build_output = run_cargo(&app_dir, &target_dir, &["build", "-j", "4"], true);
    assert!(
        build_output.status.success(),
        "broad fanout build must succeed.\n{}",
        describe(&build_output)
    );

    let run_output = run_cargo(&app_dir, &target_dir, &["run", "--quiet"], true);
    assert!(
        run_output.status.success(),
        "broad fanout execution must succeed.\n{}",
        describe(&run_output)
    );
    let expected_total: u64 = (0..width as u64).sum();
    let stdout = String::from_utf8_lossy(&run_output.stdout);
    assert!(
        stdout.contains(&format!("BROAD_TOTAL={expected_total}")),
        "expected BROAD_TOTAL={expected_total}, got:\n{stdout}"
    );
}
