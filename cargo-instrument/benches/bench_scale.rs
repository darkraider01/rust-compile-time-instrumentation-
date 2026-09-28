//! Benchmark suite for large dependency graph validation and performance budgets (P2.5).
//!
//! Runnable on stable Rust via `cargo bench --bench bench_scale`.
//!
//! Evaluates:
//! 1. Decoupled graph planning time (`SessionPlan::from_metadata_json_scoped`) across:
//!    - Broad graphs (10, 50, 100, 200 units)
//!    - Deep linear chains (10, 50, 100, 200 units)
//!    - Layered diamond multi-path graphs (10, 50, 100, 200 units)
//! 2. Public dependency build overhead for 30 unowned path dependencies.
//! 3. Evaluates against defined P2.5 empirical performance budgets.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use cargo_instrument::session::SessionPlan;

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

fn min_val(values: &[f64]) -> f64 {
    values.iter().cloned().fold(f64::INFINITY, f64::min)
}

fn max_val(values: &[f64]) -> f64 {
    values.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
}

fn make_broad_metadata(width: usize) -> serde_json::Value {
    let mut packages = Vec::with_capacity(width + 2);
    let mut resolve_nodes = Vec::with_capacity(width + 2);
    let mut app_deps = Vec::with_capacity(width + 1);

    // Root app
    let app_id = "app 0.1.0 (path+file:///app)".to_string();
    packages.push(serde_json::json!({
        "id": app_id,
        "name": "app",
        "version": "0.1.0",
        "manifest_path": "/app/Cargo.toml",
        "targets": [{"kind": ["bin"], "name": "app"}],
        "source": null
    }));

    // Shim provider
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

    // Width leaf crates
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

fn benchmark_plan(
    name: &str,
    units: usize,
    metadata: &serde_json::Value,
    iterations: usize,
) -> (f64, f64, f64, f64) {
    let mut times_ms = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = Instant::now();
        let plan = SessionPlan::from_metadata_json(metadata).expect("build session plan");
        assert!(plan.has_otel_shim_provider());
        times_ms.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let med = median(times_ms.clone());
    let min = min_val(&times_ms);
    let max = max_val(&times_ms);
    let us_per_unit = (med * 1000.0) / (units as f64);
    println!(
        "| {:<24} | {:>6} | {:>7.3} ms | {:>7.3} ms | {:>7.3} ms | {:>8.2} µs |",
        name, units, med, min, max, us_per_unit
    );
    (med, min, max, us_per_unit)
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directory");
    }
    fs::write(path, content).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

fn collect_native_scopes(path: &Path, scopes: &mut std::collections::HashSet<String>) {
    for entry in fs::read_dir(path).expect("read mirror directory") {
        let path = entry.expect("read mirror entry").path();
        if path.is_dir() {
            collect_native_scopes(&path, scopes);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let source = fs::read_to_string(&path).expect("read mirrored source");
            for scope in 0..30 {
                let name = format!("leaf_{scope}");
                if source.contains(&format!("opentelemetry::global::tracer(\"{name}\")")) {
                    scopes.insert(name);
                }
            }
        }
    }
}

fn main() {
    println!("===============================================================================");
    println!(" CARGO-INSTRUMENT SCALE & PERFORMANCE BUDGET BENCHMARK (P2.5)");
    println!("===============================================================================\n");

    println!("-------------------------------------------------------------------------------");
    println!(" PART 1: DECOUPLED SESSION GRAPH PLANNING (N=10 runs per shape/size)");
    println!("-------------------------------------------------------------------------------");
    println!(
        "| Graph Topology Shape     | Units  |  Median   |    Min    |    Max    | Per Unit  |"
    );
    println!(
        "|--------------------------|--------|-----------|-----------|-----------|-----------|"
    );

    const N: usize = 10;
    let mut planning_rates = Vec::new();

    // 1. Broad Graphs (Wide Fan-out)
    for &width in &[10, 50, 100, 200] {
        let meta = make_broad_metadata(width);
        planning_rates
            .push(benchmark_plan(&format!("Broad Fan-Out ({width})"), width + 2, &meta, N).3);
    }

    // 2. Deep Linear Chains
    for &depth in &[10, 50, 100, 200] {
        let meta = make_deep_metadata(depth);
        planning_rates
            .push(benchmark_plan(&format!("Deep Linear Chain ({depth})"), depth + 2, &meta, N).3);
    }

    // 3. Layered Diamond Graphs
    for &(layers, width) in &[(2, 5), (5, 10), (10, 10), (20, 10)] {
        let total = layers * width;
        let meta = make_diamond_metadata(layers, width);
        planning_rates
            .push(benchmark_plan(&format!("Layered Diamond ({total})"), total + 2, &meta, N).3);
    }

    println!("\n-------------------------------------------------------------------------------");
    println!(" PART 2: LIVE CARGO COMPILATION OVERHEAD AT SCALE");
    println!("-------------------------------------------------------------------------------");

    let temp = tempfile::tempdir().expect("tempdir");
    let bench_root = temp.path();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root")
        .to_path_buf();
    let otel_shim_path = repo_root
        .join("otel-shim")
        .to_string_lossy()
        .replace('\\', "/");
    let wrapper_bin = env!("CARGO_BIN_EXE_cargo-instrument");

    // Build a live broad workspace (30 leaf units + 1 app)
    let broad_dir = bench_root.join("broad_ws");
    let mut ws_members = Vec::new();
    let excluded: Vec<String> = (0..30).map(|i| format!("\"leaf_{i}\"")).collect();
    let broad_width = 30;
    for i in 0..broad_width {
        let name = format!("leaf_{i}");

        let dir = broad_dir.join(&name);
        write_file(
            &dir.join("Cargo.toml"),
            &format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        );
        write_file(
            &dir.join("src").join("lib.rs"),
            &format!("pub fn compute_{name}(x: u64) -> u64 {{ x + {i} }}\n"),
        );
    }

    let mut app_deps =
        format!("otel-shim = {{ path = \"{otel_shim_path}\" }}\nopentelemetry = \"=0.32.0\"\n");
    let mut app_calls = String::new();
    for i in 0..broad_width {
        let name = format!("leaf_{i}");
        app_deps.push_str(&format!("{name} = {{ path = \"../{name}\" }}\n"));
        app_calls.push_str(&format!("    sum += {name}::compute_{name}(1);\n"));
    }
    let app_dir = broad_dir.join("broad_app");
    ws_members.push("\"broad_app\"".to_string());
    write_file(
        &app_dir.join("Cargo.toml"),
        &format!("[package]\nname = \"broad_app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{app_deps}"),
    );
    write_file(
        &app_dir.join("src").join("main.rs"),
        &format!("fn main() {{\n    otel_shim::init();\n    let mut sum: u64 = 0;\n{app_calls}    println!(\"SUM={{sum}}\");\n}}\n"),
    );
    write_file(
        &broad_dir.join("Cargo.toml"),
        &format!(
            "[workspace]\nmembers = [\n    {}\n]\nexclude = [{}]\nresolver = \"2\"\n",
            ws_members.join(",\n    "),
            excluded.join(", ")
        ),
    );

    // Resolve dependencies once before timing and keep all measured builds offline.
    assert!(Command::new("cargo")
        .args(["fetch", "--offline"])
        .current_dir(&broad_dir)
        .status()
        .expect("fetch fixture dependencies")
        .success());
    let build = |target: &Path, instrumented: bool| {
        let mut cmd = if instrumented {
            let mut cmd = Command::new(wrapper_bin);
            cmd.args(["instrument", "--with-dependencies", "--", "build"]);
            cmd
        } else {
            let mut cmd = Command::new("cargo");
            cmd.arg("build");
            cmd
        };
        cmd.arg("--target-dir")
            .arg(target)
            .args(["--offline", "--locked"])
            .current_dir(&broad_dir)
            .env("CARGO_TERM_COLOR", "never")
            .env("CARGO_NET_OFFLINE", "true")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .env_remove("CARGO_INSTRUMENT_DEPENDENCIES");
        let start = Instant::now();
        let output = cmd.output().expect("run build");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        start.elapsed().as_secs_f64()
    };
    let mut baseline = Vec::new();
    let mut instrumented = Vec::new();
    let mut repeats = Vec::new();
    for sample in 0..5 {
        let base_target = broad_dir.join(format!("target_base_{sample}"));
        let inst_target = broad_dir.join(format!("target_inst_{sample}"));
        // Alternate order to reduce systematic filesystem cache bias.
        if sample % 2 == 0 {
            baseline.push(build(&base_target, false));
            instrumented.push(build(&inst_target, true));
        } else {
            instrumented.push(build(&inst_target, true));
            baseline.push(build(&base_target, false));
        }
        repeats.push(build(&inst_target, true));
        let mirror_root = inst_target.join("debug/deps/instrumented_sources");
        let mirrors = fs::read_dir(&mirror_root)
            .expect("dependency mirrors must exist")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .count();
        assert!(
            mirrors >= broad_width,
            "expected mirrors for all leaf dependencies, got {mirrors}"
        );
        let mut scopes = std::collections::HashSet::new();
        collect_native_scopes(&mirror_root, &mut scopes);
        assert_eq!(
            scopes.len(),
            broad_width,
            "every leaf must use native instrumentation"
        );
        println!(
            "Completed build pair {} of 5; verified {} native leaf scopes",
            sample + 1,
            scopes.len()
        );
    }
    println!("Baseline clean samples (s): {baseline:?}");
    println!("Public dependency clean samples (s): {instrumented:?}");
    println!("Public dependency repeat samples (s): {repeats:?}");
    let delta_pct = (median(instrumented) / median(baseline) - 1.0) * 100.0;
    let repeat_s = median(repeats);
    let planning_rate = max_val(&planning_rates);
    let mut failed = false;
    for (name, observed, limit) in [
        ("Planning µs/package", planning_rate, 100.0),
        ("Clean build overhead %", delta_pct, 55.0),
        ("Repeat build seconds", repeat_s, 1.5),
    ] {
        let passed = observed <= limit;
        println!(
            "{name}: observed={observed:.3}, limit={limit:.3}, {}",
            if passed { "PASS" } else { "FAIL" }
        );
        failed |= !passed;
    }
    assert!(
        !failed,
        "performance budget exceeded; retain measurements and investigate before accepting"
    );
}
