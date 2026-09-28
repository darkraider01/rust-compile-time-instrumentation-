//! P2.5 Live scale, topology, and incremental validation test suite.
//!
//! Validates:
//! 1. Public CLI workflow: `cargo instrument --with-dependencies -- ...`
//! 2. Unowned dependency graphs (strictly outside Cargo workspace membership via `exclude`)
//!    across broad (100 units), deep (linear chain), and layered (diamond multi-path) topologies.
//! 3. Generated-source checks verifying AST transformation and tracer/trampoline injection.
//! 4. Representative exported spans verified via InMemorySpanExporter and runtime execution.
//! 5. Incremental build lifecycles: clean, repeat/no-op, app-only edit, dep-only edit, clean rebuild.
//! 6. Parallel compilation and mirror isolation.
//! 7. Source, manifest, and lockfile immutability.
//! 8. First-party lint-apply scale measurement using the pinned nightly driver.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cargo-instrument parent is repo root")
        .to_path_buf()
}

fn otel_shim_path() -> String {
    repo_root()
        .join("otel-shim")
        .to_string_lossy()
        .replace('\\', "/")
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dir");
    }
    fs::write(path, content).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root).unwrap().flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|name| name == "target") {
            continue;
        }
        if path.is_dir() {
            files.extend(snapshot(&path));
        } else {
            files.push((path.clone(), fs::read(path).unwrap()));
        }
    }
    files.sort();
    files
}

fn run_cli(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cargo-instrument"))
        .args(args)
        .current_dir(root)
        .env("CARGO_TERM_COLOR", "never")
        .env("CARGO_NET_OFFLINE", "true")
        .env("INSTRUMENT_DEBUG", "1")
        .env_remove("CARGO_INSTRUMENT_DEPENDENCIES")
        .env_remove("CARGO_INSTRUMENT_REGISTRY")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("CARGO_INSTRUMENT_SESSION")
        .output()
        .unwrap()
}

fn assert_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "[{context}] failed with exit code {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn compiled_packages(output: &Output) -> HashSet<String> {
    let re_strip = |s: &str| -> String {
        let mut res = String::new();
        let mut in_escape = false;
        for c in s.chars() {
            if c == '\x1b' {
                in_escape = true;
            } else if in_escape {
                if c == 'm' {
                    in_escape = false;
                }
            } else {
                res.push(c);
            }
        }
        res
    };

    String::from_utf8_lossy(&output.stderr)
        .lines()
        .chain(String::from_utf8_lossy(&output.stdout).lines())
        .map(re_strip)
        .filter_map(|line| {
            let trimmed = line.trim();
            if let Some(pos) = trimmed.find("Compiling ") {
                let rest = &trimmed[pos + "Compiling ".len()..];
                rest.split_whitespace().next().map(str::to_owned)
            } else {
                None
            }
        })
        .collect()
}

fn run_verbose(root: &Path) -> Output {
    run_cli(
        root,
        &[
            "--with-dependencies",
            "--",
            "run",
            "--color",
            "never",
            "--verbose",
            "--offline",
            "--",
            "--with-dependencies",
        ],
    )
}

fn find_mirror_root(target_dir: &Path) -> PathBuf {
    let instrumented = target_dir.join("instrumented/debug/deps/instrumented_sources");
    if instrumented.is_dir() {
        return instrumented;
    }
    target_dir.join("debug/deps/instrumented_sources")
}

fn count_mirrors(target_dir: &Path) -> usize {
    let mirror_root = find_mirror_root(target_dir);
    if !mirror_root.is_dir() {
        return 0;
    }
    fs::read_dir(&mirror_root)
        .expect("read mirror root")
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .count()
}

fn verify_native_mirrors(target_dir: &Path, expected_scopes: &[&str]) {
    let mirror_root = find_mirror_root(target_dir);
    assert!(mirror_root.is_dir(), "mirror directory must exist");

    let mut found_scopes = HashSet::new();
    fn search_dir(dir: &Path, found: &mut HashSet<String>) {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    search_dir(&p, found);
                } else if p.extension().is_some_and(|ext| ext == "rs") {
                    if let Ok(src) = fs::read_to_string(&p) {
                        for line in src.lines() {
                            let prefix = "opentelemetry::global::tracer(\"";
                            if let Some(pos) = line.find(prefix) {
                                let rest = &line[pos + prefix.len()..];
                                if let Some(end) = rest.find('"') {
                                    found.insert(rest[..end].to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    search_dir(&mirror_root, &mut found_scopes);
    for scope in expected_scopes {
        assert!(
            found_scopes.contains(*scope),
            "expected native instrumentation scope '{scope}' in mirrors, found: {:?}",
            found_scopes
        );
    }
}

fn mirror_contains_text(dir: &Path, text: &str) -> bool {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                if mirror_contains_text(&p, text) {
                    return true;
                }
            } else if p.extension().is_some_and(|ext| ext == "rs") {
                if let Ok(content) = fs::read_to_string(&p) {
                    if content.contains(text) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

// ----------------------------------------------------------------------------
// 1. Broad Graph: 100 unowned leaf dependencies outside workspace membership
// ----------------------------------------------------------------------------
#[test]
#[serial_test::serial]
fn test_public_workflow_broad_graph_100_unowned_dependencies_execution_and_spans() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let shim_path = otel_shim_path();
    let width = 100;

    let mut excluded_members = Vec::new();
    for i in 0..width {
        let name = format!("leaf_{i}");
        excluded_members.push(format!("\"{name}\""));
        let dir = root.join(&name);
        write_file(
            &dir.join("Cargo.toml"),
            &format!("[package]\nname=\"{name}\"\nversion=\"0.1.0\"\nedition=\"2021\"\n"),
        );
        write_file(
            &dir.join("src/lib.rs"),
            &format!("pub fn compute_{name}(x: u64) -> u64 {{ x + {i} + 1 }}\n"),
        );
    }

    let mut app_deps = format!(
        "otel-shim = {{ path = \"{shim_path}\" }}\n\
         opentelemetry = \"=0.32.0\"\n\
         opentelemetry_sdk = {{ version = \"0.32.0\", features = [\"testing\"] }}\n"
    );
    let mut app_calls = String::new();
    let mut scope_assertions = String::new();

    for i in 0..width {
        let name = format!("leaf_{i}");
        app_deps.push_str(&format!("{name} = {{ path = \"../{name}\" }}\n"));
        app_calls.push_str(&format!("    sum += {name}::compute_{name}(0);\n"));
        scope_assertions.push_str(&format!(
            "    assert!(spans.iter().any(|s| s.name == \"compute_{name}\" && s.instrumentation_scope.name() == \"{name}\"), \"missing span for {name}\");\n"
        ));
    }

    let app_dir = root.join("broad_app");
    write_file(
        &app_dir.join("Cargo.toml"),
        &format!("[package]\nname=\"broad_app\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\n{app_deps}"),
    );

    let main_code = format!(
        r#"use opentelemetry::trace::{{TraceContextExt as _, Tracer as _}};
use opentelemetry_sdk::trace::{{InMemorySpanExporter, SdkTracerProvider}};

fn main() {{
    otel_shim::init();
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    opentelemetry::global::set_tracer_provider(provider);

    let tracer = opentelemetry::global::tracer("broad_app");
    let root_span = tracer.start("parent_batch");
    let cx = opentelemetry::Context::current_with_span(root_span);
    let _guard = cx.attach();

    let mut sum: u64 = 0;
{app_calls}
    println!("BROAD_SUM={{sum}}");
    assert_eq!(otel_shim::active_span_count(), 0);

    let spans = exporter.get_finished_spans().expect("get spans");
    assert!(spans.len() >= {width}, "expected at least {width} spans, got {{}}", spans.len());
{scope_assertions}
    println!("BROAD_100_VERIFIED");
}}
"#
    );
    write_file(&app_dir.join("src/main.rs"), &main_code);

    write_file(
        &root.join("Cargo.toml"),
        &format!(
            "[workspace]\nmembers = [\"broad_app\"]\nexclude = [{}]\nresolver = \"2\"\n",
            excluded_members.join(", ")
        ),
    );

    let fetch_out = Command::new("cargo")
        .args(["fetch", "--offline"])
        .current_dir(root)
        .output()
        .unwrap();
    assert_success(&fetch_out, "cargo fetch");

    let before_snapshot = snapshot(root);

    let output = run_cli(
        root,
        &[
            "--with-dependencies",
            "--",
            "run",
            "-j",
            "4",
            "--offline",
            "--",
            "--with-dependencies",
        ],
    );
    assert_success(&output, "public broad 100 run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("BROAD_100_VERIFIED"),
        "must execute and verify all 100 spans:\n{stdout}"
    );

    let expected_sum: u64 = (1..=width as u64).sum();
    assert!(
        stdout.contains(&format!("BROAD_SUM={expected_sum}")),
        "expected BROAD_SUM={expected_sum}:\n{stdout}"
    );

    let target_dir = root.join("target");
    let mirrors = count_mirrors(&target_dir);
    assert!(
        mirrors >= width,
        "expected at least {width} mirror directories, found {mirrors}"
    );

    let sample_scopes: Vec<String> = (0..width).map(|i| format!("leaf_{i}")).collect();
    let scope_refs: Vec<&str> = sample_scopes.iter().map(|s| s.as_str()).collect();
    verify_native_mirrors(&target_dir, &scope_refs);

    let after_snapshot = snapshot(root);
    assert_eq!(
        before_snapshot, after_snapshot,
        "input files must remain strictly byte-immutable after 100-dependency build"
    );
}

// ----------------------------------------------------------------------------
// 2. Layered Diamond Graph: 100 unowned dependencies in a multi-path graph
// ----------------------------------------------------------------------------
#[test]
#[serial_test::serial]
fn test_public_workflow_layered_diamond_100_unowned_dependencies() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let shim_path = otel_shim_path();

    // 4 layers of 25 crates each = 100 unowned crates outside workspace
    let layers = 4;
    let width = 25;
    let mut excluded_members = Vec::new();

    for layer in 0..layers {
        for i in 0..width {
            let crate_name = format!("diamond_l{layer}_{i}");
            excluded_members.push(format!("\"{crate_name}\""));
            let dir = root.join(&crate_name);

            let (deps, body) = if layer == 0 {
                (
                    String::new(),
                    format!(
                        "pub fn compute_{crate_name}(val: u64) -> u64 {{ val + {} }}\n",
                        i + 1
                    ),
                )
            } else {
                let dep1 = format!("diamond_l{}_{}", layer - 1, i);
                let dep2 = format!("diamond_l{}_{}", layer - 1, (i + 1) % width);
                (
                    format!(
                        "{dep1} = {{ path = \"../{dep1}\" }}\n\
                         {dep2} = {{ path = \"../{dep2}\" }}\n"
                    ),
                    format!(
                        "pub fn compute_{crate_name}(val: u64) -> u64 {{\n    \
                            {dep1}::compute_{dep1}(val) + {dep2}::compute_{dep2}(val) + 1\n\
                        }}\n"
                    ),
                )
            };

            write_file(
                &dir.join("Cargo.toml"),
                &format!("[package]\nname=\"{crate_name}\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\n{deps}"),
            );
            write_file(&dir.join("src/lib.rs"), &body);
        }
    }

    let top_layer = layers - 1;
    let mut app_deps = format!(
        "otel-shim = {{ path = \"{shim_path}\" }}\n\
         opentelemetry = \"=0.32.0\"\n"
    );
    let mut app_calls = String::new();
    for i in 0..width {
        let top_dep = format!("diamond_l{top_layer}_{i}");
        app_deps.push_str(&format!("{top_dep} = {{ path = \"../{top_dep}\" }}\n"));
        app_calls.push_str(&format!("    total += {top_dep}::compute_{top_dep}(0);\n"));
    }

    let app_dir = root.join("diamond_app");
    write_file(
        &app_dir.join("Cargo.toml"),
        &format!("[package]\nname=\"diamond_app\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\n{app_deps}"),
    );
    write_file(
        &app_dir.join("src/main.rs"),
        &format!(
            "fn main() {{\n    otel_shim::init();\n    let mut total: u64 = 0;\n{app_calls}    println!(\"DIAMOND_RESULT={{total}}\");\n}}\n"
        ),
    );

    write_file(
        &root.join("Cargo.toml"),
        &format!(
            "[workspace]\nmembers = [\"diamond_app\"]\nexclude = [{}]\nresolver = \"2\"\n",
            excluded_members.join(", ")
        ),
    );

    let fetch_out = Command::new("cargo")
        .args(["fetch", "--offline"])
        .current_dir(root)
        .output()
        .unwrap();
    assert_success(&fetch_out, "cargo fetch diamond");

    let before_snapshot = snapshot(root);

    let output = run_cli(
        root,
        &[
            "--with-dependencies",
            "--",
            "run",
            "-j",
            "4",
            "--offline",
            "--",
            "--with-dependencies",
        ],
    );
    assert_success(&output, "public diamond run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("DIAMOND_RESULT="),
        "diamond application must execute successfully:\n{stdout}"
    );

    let target_dir = root.join("target");
    let mirrors = count_mirrors(&target_dir);
    assert!(
        mirrors >= layers * width,
        "expected at least 100 mirror directories, found {mirrors}"
    );

    let sample_scopes = [
        "diamond_l0_0",
        "diamond_l0_12",
        "diamond_l1_5",
        "diamond_l2_10",
        "diamond_l3_24",
    ];
    verify_native_mirrors(&target_dir, &sample_scopes);

    assert_eq!(
        before_snapshot,
        snapshot(root),
        "diamond sources must remain byte-identical"
    );
}

// ----------------------------------------------------------------------------
// 3. Deep Linear Chain: 20 unowned crates in strict transitive chain
// ----------------------------------------------------------------------------
#[test]
#[serial_test::serial]
fn test_public_workflow_deep_linear_chain_unowned_dependencies() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let shim_path = otel_shim_path();
    let depth = 20;

    let mut excluded_members = Vec::new();
    for i in 0..depth {
        let name = format!("chain_{i}");
        excluded_members.push(format!("\"{name}\""));
        let dir = root.join(&name);

        let (deps, body) = if i == 0 {
            (
                String::new(),
                format!("pub fn step_{name}(v: u64) -> u64 {{ v + 1 }}\n"),
            )
        } else {
            let prev = format!("chain_{}", i - 1);
            (
                format!("{prev} = {{ path = \"../{prev}\" }}\n"),
                format!(
                    "pub fn step_{name}(v: u64) -> u64 {{\n    {prev}::step_{prev}(v) + 1\n}}\n"
                ),
            )
        };

        write_file(
            &dir.join("Cargo.toml"),
            &format!("[package]\nname=\"{name}\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\n{deps}"),
        );
        write_file(&dir.join("src/lib.rs"), &body);
    }

    let last_dep = format!("chain_{}", depth - 1);
    let app_dir = root.join("chain_app");
    write_file(
        &app_dir.join("Cargo.toml"),
        &format!(
            "[package]\nname=\"chain_app\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n\
             [dependencies]\n\
             {last_dep} = {{ path = \"../{last_dep}\" }}\n\
             otel-shim = {{ path = \"{shim_path}\" }}\n\
             opentelemetry = \"=0.32.0\"\n"
        ),
    );
    write_file(
        &app_dir.join("src/main.rs"),
        &format!(
            "fn main() {{\n    otel_shim::init();\n    let val = {last_dep}::step_{last_dep}(0);\n    println!(\"CHAIN_RESULT={{val}}\");\n}}\n"
        ),
    );

    write_file(
        &root.join("Cargo.toml"),
        &format!(
            "[workspace]\nmembers = [\"chain_app\"]\nexclude = [{}]\nresolver = \"2\"\n",
            excluded_members.join(", ")
        ),
    );

    let fetch_out = Command::new("cargo")
        .args(["fetch", "--offline"])
        .current_dir(root)
        .output()
        .unwrap();
    assert_success(&fetch_out, "cargo fetch deep chain");

    let output = run_cli(
        root,
        &[
            "--with-dependencies",
            "--",
            "run",
            "--offline",
            "--",
            "--with-dependencies",
        ],
    );
    assert_success(&output, "public deep chain run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("CHAIN_RESULT={depth}")),
        "expected CHAIN_RESULT={depth}:\n{stdout}"
    );

    let target_dir = root.join("target");
    let mirrors = count_mirrors(&target_dir);
    assert!(
        mirrors >= depth,
        "expected at least {depth} mirrors for deep chain, got {mirrors}"
    );

    let sample_scopes = ["chain_0", "chain_5", "chain_10", "chain_19"];
    verify_native_mirrors(&target_dir, &sample_scopes);
}

// ----------------------------------------------------------------------------
// 4. Incremental Lifecycles: clean, repeat, app-only edit, dep-only edit, clean rebuild
// ----------------------------------------------------------------------------
#[test]
#[serial_test::serial]
fn test_public_workflow_incremental_cycles_at_scale() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let shim_path = otel_shim_path();
    let width = 10;

    let mut excluded = Vec::new();
    for i in 0..width {
        let name = format!("inc_leaf_{i}");
        excluded.push(format!("\"{name}\""));
        let dir = root.join(&name);
        write_file(
            &dir.join("Cargo.toml"),
            &format!("[package]\nname=\"{name}\"\nversion=\"0.1.0\"\nedition=\"2021\"\n"),
        );
        write_file(
            &dir.join("src/lib.rs"),
            &format!("pub fn work_{name}(x: u32) -> u32 {{ x + {i} }}\n"),
        );
    }

    let mut app_deps = format!(
        "otel-shim = {{ path = \"{shim_path}\" }}\n\
         opentelemetry = \"=0.32.0\"\n"
    );
    let mut app_calls = String::new();
    for i in 0..width {
        let name = format!("inc_leaf_{i}");
        app_deps.push_str(&format!("{name} = {{ path = \"../{name}\" }}\n"));
        app_calls.push_str(&format!("    sum += {name}::work_{name}(1);\n"));
    }

    let app_dir = root.join("inc_app");
    write_file(
        &app_dir.join("Cargo.toml"),
        &format!("[package]\nname=\"inc_app\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\n{app_deps}"),
    );
    let main_template = format!(
        "fn main() {{\n    otel_shim::init();\n    let mut sum: u32 = 0;\n{app_calls}    println!(\"INC_SUM={{sum}}\");\n}}\n"
    );
    write_file(&app_dir.join("src/main.rs"), &main_template);

    write_file(
        &root.join("Cargo.toml"),
        &format!(
            "[workspace]\nmembers = [\"inc_app\"]\nexclude = [{}]\nresolver = \"2\"\n",
            excluded.join(", ")
        ),
    );

    let fetch_out = Command::new("cargo")
        .args(["fetch", "--offline"])
        .current_dir(root)
        .output()
        .unwrap();
    assert_success(&fetch_out, "cargo fetch inc");

    let original_snapshot = snapshot(root);

    // Cycle 1: Clean build
    let out = run_verbose(root);
    assert_success(&out, "cycle 1: clean build");
    assert!(String::from_utf8_lossy(&out.stdout).contains("INC_SUM=55"));
    assert_eq!(count_mirrors(&root.join("target")), width);
    let initially_compiled = compiled_packages(&out);
    for i in 0..width {
        assert!(
            initially_compiled.contains(&format!("inc_leaf_{i}")),
            "initial build must compile inc_leaf_{i}: {initially_compiled:?}; stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert_eq!(
        original_snapshot,
        snapshot(root),
        "clean build immutability"
    );

    // Cycle 2: Repeat build (no-op)
    let start_repeat = Instant::now();
    let out_repeat = run_verbose(root);
    let repeat_elapsed = start_repeat.elapsed();
    assert_success(&out_repeat, "cycle 2: repeat build");
    assert!(String::from_utf8_lossy(&out_repeat.stdout).contains("INC_SUM=55"));
    assert!(
        compiled_packages(&out_repeat).is_disjoint(&initially_compiled),
        "repeat build recompiled packages: {:?}",
        compiled_packages(&out_repeat)
    );
    assert_eq!(
        original_snapshot,
        snapshot(root),
        "repeat build immutability"
    );
    println!(
        "Repeat build completed in {:.3}s",
        repeat_elapsed.as_secs_f64()
    );

    // Cycle 3: Application-only edit
    let modified_app_main = main_template.replace("INC_SUM=", "INC_EDIT=");
    write_file(&app_dir.join("src/main.rs"), &modified_app_main);
    std::thread::sleep(std::time::Duration::from_secs(1));
    let app_snapshot = snapshot(root);
    let out_app_edit = run_verbose(root);
    assert_success(&out_app_edit, "cycle 3: app-only edit");
    assert!(String::from_utf8_lossy(&out_app_edit.stdout).contains("INC_EDIT=55"));
    let app_compiled = compiled_packages(&out_app_edit);
    assert!(
        String::from_utf8_lossy(&out_app_edit.stdout).contains("INC_EDIT=55"),
        "edited application should run its changed code: stdout={}",
        String::from_utf8_lossy(&out_app_edit.stdout)
    );
    assert!(
        app_compiled
            .iter()
            .all(|name| !name.starts_with("inc_leaf_")),
        "app edit must not rebuild dependencies: {app_compiled:?}"
    );
    assert_eq!(
        app_snapshot,
        snapshot(root),
        "app-only build must not modify dependency inputs"
    );
    assert_eq!(count_mirrors(&root.join("target")), width);

    // Cycle 4: Dependency-only edit
    let modified_dep = "pub fn work_inc_leaf_0(x: u32) -> u32 { x + 999 }\n";
    write_file(&root.join("inc_leaf_0/src/lib.rs"), modified_dep);
    std::thread::sleep(std::time::Duration::from_secs(1));
    let dep_snapshot = snapshot(root);
    let out_dep_edit = run_verbose(root);
    assert_success(&out_dep_edit, "cycle 4: dep-only edit");
    let stdout_dep = String::from_utf8_lossy(&out_dep_edit.stdout);
    assert!(
        stdout_dep.contains("INC_EDIT=1054"),
        "dependency edit must affect the program result: {stdout_dep}"
    );
    let dep_compiled = compiled_packages(&out_dep_edit);
    assert!(
        dep_compiled.contains("inc_leaf_0"),
        "edited dependency should rebuild: {dep_compiled:?}; stderr={}",
        String::from_utf8_lossy(&out_dep_edit.stderr)
    );
    assert!(
        dep_compiled
            .iter()
            .all(|name| !name.starts_with("inc_leaf_") || name == "inc_leaf_0"),
        "unmodified dependencies should remain fresh: {dep_compiled:?}"
    );
    assert_eq!(
        dep_snapshot,
        snapshot(root),
        "dependency build must not modify other inputs"
    );
    let mirror_dir = find_mirror_root(&root.join("target"));
    assert!(
        mirror_contains_text(&mirror_dir, "999"),
        "modified dependency must be re-instrumented into mirror"
    );

    // Cycle 5: Clean rebuild
    let clean_out = Command::new("cargo")
        .args(["clean"])
        .current_dir(root)
        .output()
        .unwrap();
    assert_success(&clean_out, "cargo clean");
    let out_rebuild = run_verbose(root);
    assert_success(&out_rebuild, "cycle 5: clean rebuild");
    assert!(String::from_utf8_lossy(&out_rebuild.stdout).contains("INC_EDIT=1054"));
    let rebuilt = compiled_packages(&out_rebuild);
    for i in 0..width {
        assert!(
            rebuilt.contains(&format!("inc_leaf_{i}")),
            "clean rebuild must compile inc_leaf_{i}: {rebuilt:?}"
        );
    }
    assert_eq!(count_mirrors(&root.join("target")), width);
}

// ----------------------------------------------------------------------------
// 5. Tier-2 Fallback at Scale: Unowned dependencies without opentelemetry linkage
// ----------------------------------------------------------------------------
#[test]
#[serial_test::serial]
fn test_public_workflow_tier2_fallback_at_scale() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let shim_path = otel_shim_path();
    let width = 10;

    let mut excluded = Vec::new();
    for i in 0..width {
        let name = format!("fallback_leaf_{i}");
        excluded.push(format!("\"{name}\""));
        let dir = root.join(&name);
        write_file(
            &dir.join("Cargo.toml"),
            &format!("[package]\nname=\"{name}\"\nversion=\"0.1.0\"\nedition=\"2021\"\n"),
        );
        write_file(
            &dir.join("src/lib.rs"),
            &format!("#![deny(warnings)]\npub fn fallback_work_{i}() -> u32 {{ {i} + 10 }}\n"),
        );
    }

    let mut app_deps = format!(
        "otel-shim = {{ path = \"{shim_path}\" }}\n\
         opentelemetry = \"=0.32.0\"\n\
         opentelemetry_sdk = {{ version = \"0.32.0\", features = [\"testing\"] }}\n"
    );
    let mut profile_overrides = String::new();
    let mut app_calls = String::new();
    for i in 0..width {
        let name = format!("fallback_leaf_{i}");
        app_deps.push_str(&format!("{name} = {{ path = \"../{name}\" }}\n"));
        app_calls.push_str(&format!("    val += {name}::fallback_work_{i}();\n"));
        profile_overrides.push_str(&format!("[profile.dev.package.{name}]\nopt-level = 1\n"));
    }

    let app_dir = root.join("fallback_app");
    write_file(
        &app_dir.join("Cargo.toml"),
        &format!("[package]\nname=\"fallback_app\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[dependencies]\n{app_deps}"),
    );
    write_file(
        &app_dir.join("src/main.rs"),
        &format!(
            r#"use opentelemetry::trace::{{TraceContextExt as _, Tracer as _}};
use opentelemetry_sdk::trace::{{InMemorySpanExporter, SdkTracerProvider}};

fn main() {{
    otel_shim::init();
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder().with_simple_exporter(exporter.clone()).build();
    opentelemetry::global::set_tracer_provider(provider);
    let cx = opentelemetry::Context::current_with_span(opentelemetry::global::tracer("manual").start("parent"));
    let mut val: u32 = 0;
    {{
        let _guard = cx.attach();
{app_calls}
    }}
    assert_eq!(otel_shim::active_span_count(), 0);
    let spans = exporter.get_finished_spans().unwrap();
    assert!(spans.len() >= {width}, "expected at least {width} spans");
    println!("FALLBACK_VAL={{val}}");
}}
"#
        ),
    );

    write_file(
        &root.join("Cargo.toml"),
        &format!(
            "[workspace]\nmembers = [\"fallback_app\"]\nexclude = [{}]\nresolver = \"2\"\n\n{profile_overrides}",
            excluded.join(", ")
        ),
    );

    let fetch_out = Command::new("cargo")
        .args(["fetch", "--offline"])
        .current_dir(root)
        .output()
        .unwrap();
    assert_success(&fetch_out, "cargo fetch fallback");

    let output = run_cli(
        root,
        &[
            "--with-dependencies",
            "--",
            "run",
            "--offline",
            "--",
            "--with-dependencies",
        ],
    );
    assert_success(&output, "public fallback run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("FALLBACK_VAL="));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("selecting Tier-2 C-ABI emitter"),
        "must select Tier-2 fallback when native opentelemetry is absent:\n{stderr}"
    );

    let mirror_root = find_mirror_root(&root.join("target"));
    assert!(mirror_root.is_dir());

    // Check trampoline generation in mirrored sources
    let mut found_trampoline = false;
    fn search_trampoline(dir: &Path, found: &mut bool) {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    search_trampoline(&p, found);
                } else if p.extension().is_some_and(|ext| ext == "rs") {
                    if let Ok(code) = fs::read_to_string(&p) {
                        if code.contains("__otel_span_enter") || code.contains("__otel_span_exit") {
                            *found = true;
                            return;
                        }
                    }
                }
            }
        }
    }
    search_trampoline(&mirror_root, &mut found_trampoline);
    assert!(
        found_trampoline,
        "mirrored files must contain C-ABI trampolines"
    );
}

// ----------------------------------------------------------------------------
// 6. First-Party Lint-Apply Scale Measurement (using pinned nightly toolchain)
// ----------------------------------------------------------------------------
#[test]
#[ignore = "requires pinned nightly-2026-09-09 toolchain with rustc-dev"]
#[serial_test::serial]
fn test_first_party_lint_apply_scale_measurement() {
    let toolchain = "nightly-2026-09-09";
    // Check if the pinned toolchain is installed
    let toolchains_out = Command::new("rustup")
        .args(["toolchain", "list"])
        .output()
        .expect("query rustup");
    let toolchains_str = String::from_utf8_lossy(&toolchains_out.stdout);
    if !toolchains_str.contains(toolchain) {
        eprintln!(
            "SKIPPING test_first_party_lint_apply_scale_measurement: '{toolchain}' not installed"
        );
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    let shim_path = otel_shim_path();
    let num_crates = 5;
    let mut members = Vec::new();

    for i in 0..num_crates {
        let name = format!("first_party_{i}");
        members.push(format!("\"{name}\""));
        let dir = root.join(&name);
        write_file(
            &dir.join("Cargo.toml"),
            &format!(
                "[package]\nname=\"{name}\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n\
                 [dependencies]\n\
                 otel-shim = {{ path = \"{shim_path}\" }}\n\
                 opentelemetry = \"=0.32.0\"\n"
            ),
        );
        let mut functions = String::new();
        for f in 0..10 {
            functions.push_str(&format!(
                "pub fn compute_{f}(x: i32) -> i32 {{ x + {f} }}\n\
                 pub async fn compute_async_{f}(x: i32) -> i32 {{ x + {f} }}\n"
            ));
        }
        write_file(&dir.join("src/lib.rs"), &functions);
    }

    write_file(
        &root.join("Cargo.toml"),
        &format!(
            "[workspace]\nmembers = [{}]\nresolver = \"2\"\n",
            members.join(", ")
        ),
    );

    write_file(&root.join(".gitignore"), "target/\n");

    // Initial git repo for cargo fix clean-tree gate
    let git_init = Command::new("git")
        .args(["init"])
        .current_dir(root)
        .output()
        .unwrap();
    assert_success(&git_init, "git init");
    Command::new("git")
        .args(["config", "user.name", "P2.3 Fixture"])
        .current_dir(root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "p23@example.invalid"])
        .current_dir(root)
        .output()
        .unwrap();
    let git_add = Command::new("git")
        .args(["add", "."])
        .current_dir(root)
        .output()
        .unwrap();
    assert_success(&git_add, "git add");
    let git_commit = Command::new("git")
        .args([
            "-c",
            "user.name=P2.3 Fixture",
            "-c",
            "user.email=p23@example.invalid",
            "commit",
            "-m",
            "init",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert_success(&git_commit, "git commit");

    let binary = Path::new(env!("CARGO_BIN_EXE_cargo-instrument-rust"));
    let mut paths = vec![binary.parent().unwrap().to_path_buf()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));

    let start = Instant::now();
    for i in 0..num_crates {
        let pkg_name = format!("first_party_{i}");
        let apply_out = Command::new("cargo")
            .args([
                "instrument-rust",
                "--apply",
                "--package",
                &pkg_name,
                "--offline",
            ])
            .current_dir(root)
            .env("PATH", std::env::join_paths(&paths).unwrap())
            .env("CARGO_INSTRUMENT_RUST_TOOLCHAIN", toolchain)
            .output()
            .unwrap();
        assert_success(
            &apply_out,
            &format!("cargo instrument-rust --apply {pkg_name}"),
        );

        // Verify markers inserted
        let src = fs::read_to_string(root.join(&pkg_name).join("src/lib.rs")).unwrap();
        assert!(
            src.contains("/* __cargo_instrument_rust:p23 */"),
            "package {pkg_name} must contain p23 markers:\n{src}"
        );

        // Commit changes so next package starts with clean tree per safety invariant
        Command::new("git")
            .args(["add", "."])
            .current_dir(root)
            .output()
            .unwrap();
        let commit_res = Command::new("git")
            .args([
                "-c",
                "user.name=P2.3 Fixture",
                "-c",
                "user.email=p23@example.invalid",
                "commit",
                "-m",
                &format!("apply {pkg_name}"),
            ])
            .current_dir(root)
            .output()
            .unwrap();
        assert_success(&commit_res, "git commit apply");
    }
    let elapsed = start.elapsed();
    println!(
        "First-party lint-apply completed for {num_crates} crates (100 functions total) in {:.3}s ({:.2} ms/crate)",
        elapsed.as_secs_f64(),
        elapsed.as_secs_f64() * 1000.0 / (num_crates as f64)
    );

    // Verify idempotency (zero edits on second run against clean tree)

    let second_apply = Command::new("cargo")
        .args([
            "instrument-rust",
            "--apply",
            "--package",
            "first_party_0",
            "--offline",
        ])
        .current_dir(root)
        .env("PATH", std::env::join_paths(&paths).unwrap())
        .env("CARGO_INSTRUMENT_RUST_TOOLCHAIN", toolchain)
        .output()
        .unwrap();
    assert_success(&second_apply, "second apply run");
    let diff = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        diff.stdout.is_empty(),
        "second apply must be clean idempotent no-op:\n{}",
        String::from_utf8_lossy(&diff.stdout)
    );
}
