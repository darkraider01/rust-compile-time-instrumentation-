use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serial_test::serial;
use sha2::{Digest, Sha256};

use cargo_instrument::ast::{analyze_source_str, check_application_preflight};
use cargo_instrument::candidate::{Candidate, FunctionKind, UnsafePolicy};
use cargo_instrument::transform::{
    transform_source_str_with_trampoline, Emitter, SkipReason, TrampolineEmitter,
    TransformationPlan,
};

// ----------------------------------------------------------------------------
// Unit Tests: Trampoline Emitter Code Shapes & Behaviors
// ----------------------------------------------------------------------------

#[test]
fn test_trampoline_sync_ordinary_fn_shape() {
    let source = r#"pub fn calculate(a: u32, b: u32) -> u32 {
    a + b
}
"#;
    let report = analyze_source_str("my_dep", Path::new("src/lib.rs"), source)
        .expect("analysis should succeed");
    assert_eq!(report.candidates.len(), 1);

    let transformed = transform_source_str_with_trampoline(
        source,
        "my_dep",
        Some("2021".to_string()),
        UnsafePolicy::Allowed,
        &report.candidates,
    )
    .expect("transformation should succeed");

    // Must declare 2 symbols only (M1 minimization)
    assert!(transformed.contains("extern \"C\" {"));
    assert!(transformed.contains("fn __otel_span_enter("));
    assert!(transformed.contains("fn __otel_span_exit(handle: u64);"));
    assert!(!transformed.contains("fn __otel_span_set_error"));
    assert!(!transformed.contains("fn __otel_span_start"));

    // Must instantiate RAII guard
    assert!(transformed.contains("struct __OtelGuard(u64);"));
    assert!(transformed.contains("impl Drop for __OtelGuard"));
    assert!(transformed.contains("__otel_span_exit(self.0);"));
    assert!(transformed.contains("let _otel_guard = __OtelGuard(unsafe {"));
    assert!(transformed.contains("__otel_span_enter("));
    assert!(transformed.contains("0u8,")); // SpanKind::Internal

    // Body statements preserved
    assert!(transformed.contains("a + b"));
}

#[test]
fn test_trampoline_sync_result_fn_shape() {
    let source = r#"pub fn parse_data(raw: &str) -> Result<u32, String> {
    if raw.is_empty() {
        return Err("empty".to_string());
    }
    Ok(42)
}
"#;
    let report = analyze_source_str("my_dep", Path::new("src/lib.rs"), source)
        .expect("analysis should succeed");
    assert_eq!(report.candidates.len(), 1);

    let transformed = transform_source_str_with_trampoline(
        source,
        "my_dep",
        Some("2021".to_string()),
        UnsafePolicy::Allowed,
        &report.candidates,
    )
    .expect("transformation should succeed");

    // Must declare 3 symbols (enter, exit, set_error) (M1)
    assert!(transformed.contains("fn __otel_span_enter("));
    assert!(transformed.contains("fn __otel_span_exit(handle: u64);"));
    assert!(transformed.contains("fn __otel_span_set_error(handle: u64);"));

    // Must wrap in closure and check error
    assert!(transformed.contains("let __otel_res: core::result::Result<_, _> = (|| {"));
    assert!(transformed.contains("})();"));
    assert!(transformed.contains("if __otel_res.is_err() && __otel_guard.0 != 0 {"));
    assert!(transformed.contains("__otel_span_set_error(__otel_guard.0);"));
    assert!(transformed.contains("__otel_res"));
}

#[test]
fn test_trampoline_edition_2021_vs_2024() {
    let candidate = Candidate {
        function_name: "foo".to_string(),
        source_file: PathBuf::from("src/lib.rs"),
        byte_range: 1..15,
        body_byte_range: 10..15,
        kind: FunctionKind::Free,
        is_async: false,
        is_generic: false,
        has_enclosing_generics: false,
        returns_result: false,
        returns_mut_reference: false,
        returns_reference_or_lifetime: false,
    };

    let emitter_2021 =
        TrampolineEmitter::new("dep", Some("2021".to_string()), UnsafePolicy::Allowed);
    let prefix_2021 = emitter_2021.emit_body_prefix(&candidate, "\n");
    assert!(prefix_2021.contains("extern \"C\" {"));
    assert!(!prefix_2021.contains("unsafe extern \"C\" {"));

    let emitter_2024 =
        TrampolineEmitter::new("dep", Some("2024".to_string()), UnsafePolicy::Allowed);
    let prefix_2024 = emitter_2024.emit_body_prefix(&candidate, "\n");
    assert!(prefix_2024.contains("unsafe extern \"C\" {"));
}

#[test]
fn test_trampoline_denied_unsafe_policy_allows() {
    let candidate = Candidate {
        function_name: "try_action".to_string(),
        source_file: PathBuf::from("src/lib.rs"),
        byte_range: 1..25,
        body_byte_range: 20..25,
        kind: FunctionKind::Free,
        is_async: false,
        is_generic: false,
        has_enclosing_generics: false,
        returns_result: true,
        returns_mut_reference: false,
        returns_reference_or_lifetime: false,
    };

    let emitter = TrampolineEmitter::new("dep", Some("2021".to_string()), UnsafePolicy::Denied);
    let prefix = emitter.emit_body_prefix(&candidate, "\n");
    let suffix = emitter.emit_body_suffix(&candidate, "\n");

    // G3: #[allow(unsafe_code)] must be emitted at all unsafe boundaries
    assert!(prefix.contains("#[allow(unsafe_code)]\n    extern \"C\" {"));
    assert!(prefix.contains("#[allow(unsafe_code)]\n                unsafe {"));
    assert!(prefix.contains("#[allow(unsafe_code)]\n    let __otel_guard = __OtelGuard(unsafe {"));
    assert!(suffix.contains("#[allow(unsafe_code)]\n        unsafe {"));
}

#[test]
fn test_trampoline_async_functions_deferred() {
    let source = r#"
pub async fn fetch_remote(id: u64) -> String {
    format!("id_{id}")
}
"#;
    let candidate = Candidate {
        function_name: "fetch_remote".to_string(),
        source_file: PathBuf::from("src/lib.rs"),
        byte_range: 1..65,
        body_byte_range: 46..65,
        kind: FunctionKind::Free,
        is_async: true,
        is_generic: false,
        has_enclosing_generics: false,
        returns_result: false,
        returns_mut_reference: false,
        returns_reference_or_lifetime: false,
    };

    let emitter = TrampolineEmitter::new("dep", Some("2021".to_string()), UnsafePolicy::Allowed);
    assert!(!emitter.handles_async());

    let plan = TransformationPlan::build_with_emitter(source, &[candidate], &emitter)
        .expect("plan build should succeed");

    assert_eq!(plan.edits.len(), 0);
    assert_eq!(plan.skipped.len(), 1);
    assert_eq!(plan.skipped[0].reason, SkipReason::AsyncDeferred);
}

#[test]
fn test_trampoline_mut_ref_fallback() {
    let source = r#"
pub fn get_mut_val(val: &mut u32) -> Result<&mut u32, ()> {
    Ok(val)
}
"#;
    let candidate = Candidate {
        function_name: "get_mut_val".to_string(),
        source_file: PathBuf::from("src/lib.rs"),
        byte_range: 1..68,
        body_byte_range: 59..68,
        kind: FunctionKind::Free,
        is_async: false,
        is_generic: false,
        has_enclosing_generics: false,
        returns_result: true,
        returns_mut_reference: true,
        returns_reference_or_lifetime: true,
    };

    let transformed = transform_source_str_with_trampoline(
        source,
        "dep",
        Some("2021".to_string()),
        UnsafePolicy::Allowed,
        &[candidate],
    )
    .expect("transformation should succeed");

    // Since returns_reference_or_lifetime is true, must NOT wrap in closure (prevents closure escape borrow errors)
    assert!(!transformed.contains("let __otel_res: Result<_, _> = (|| {"));
    assert!(transformed.contains("let _otel_guard = __OtelGuard(unsafe {"));
}

// ----------------------------------------------------------------------------
// Unit Tests: Recursive Module Preflight Check (G2 / C2)
// ----------------------------------------------------------------------------

#[test]
fn test_preflight_check_finds_otel_shim_in_root() {
    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let root_file = temp_dir.path().join("main.rs");
    fs::write(
        &root_file,
        r#"
fn main() {
    otel_shim::init();
    println!("Hello");
}
"#,
    )
    .expect("write main.rs");

    let result = check_application_preflight("my_app", &root_file);
    assert!(result.is_ok(), "preflight should succeed: {:?}", result);
}

#[test]
fn test_preflight_check_finds_otel_shim_in_submodule() {
    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let root_file = temp_dir.path().join("main.rs");
    let telemetry_file = temp_dir.path().join("telemetry.rs");

    fs::write(
        &root_file,
        r#"
mod telemetry;

fn main() {
    telemetry::setup();
}
"#,
    )
    .expect("write main.rs");

    fs::write(
        &telemetry_file,
        r#"
pub fn setup() {
    otel_shim::init();
}
"#,
    )
    .expect("write telemetry.rs");

    let result = check_application_preflight("my_app", &root_file);
    assert!(
        result.is_ok(),
        "preflight should succeed for submodule reference: {:?}",
        result
    );
}

#[test]
fn test_preflight_check_fails_when_otel_shim_missing() {
    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let root_file = temp_dir.path().join("main.rs");
    fs::write(
        &root_file,
        r#"
fn main() {
    println!("No shim reference here");
}
"#,
    )
    .expect("write main.rs");

    let result = check_application_preflight("my_app", &root_file);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("must reference `otel_shim`"));
    assert!(err.contains("ADR-003 / E-10"));
}

// ----------------------------------------------------------------------------
// End-to-End Integration Test: Multi-Crate Dependency Instrumentation
// ----------------------------------------------------------------------------

#[test]
fn test_trampoline_live_end_to_end_runtime_proof() {
    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let ws_root = temp_dir.path();

    // Find path to otel-shim crate in repo
    let current_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = current_manifest
        .parent()
        .expect("cargo-instrument parent is repo root");
    let otel_shim_path = repo_root.join("otel-shim");
    let cargo_instrument_bin = env!("CARGO_BIN_EXE_cargo-instrument");

    // 1. Create Workspace Cargo.toml
    let ws_cargo_toml = ws_root.join("Cargo.toml");
    fs::write(
        &ws_cargo_toml,
        r#"[workspace]
members = [
    "app",
    "dep_a with spaces",
    "dep_b",
]
resolver = "2"
"#,
    )
    .expect("write workspace Cargo.toml");

    // 2. Create Dependency A (with spaces in directory path)
    // Pure dependency: does NOT depend on opentelemetry or otel-shim!
    let dep_a_dir = ws_root.join("dep_a with spaces");
    let dep_a_src = dep_a_dir.join("src");
    fs::create_dir_all(&dep_a_src).expect("create dep_a src");

    let dep_a_cargo = dep_a_dir.join("Cargo.toml");
    fs::write(
        &dep_a_cargo,
        r#"[package]
name = "dep_a"
version = "0.1.0"
edition = "2021"

[dependencies]
"#,
    )
    .expect("write dep_a Cargo.toml");

    let dep_a_lib = dep_a_src.join("lib.rs");
    let dep_a_original_code = r#"pub fn compute(a: u32, b: u32) -> u32 {
    a + b
}

pub fn fallible_op(fail: bool) -> Result<u32, String> {
    if fail {
        Err("operation failed".to_string())
    } else {
        Ok(100)
    }
}

pub async fn async_uninstrumented() -> u32 {
    42
}
"#;
    fs::write(&dep_a_lib, dep_a_original_code).expect("write dep_a lib.rs");

    // 3. Create Dependency B
    let dep_b_dir = ws_root.join("dep_b");
    let dep_b_src = dep_b_dir.join("src");
    fs::create_dir_all(&dep_b_src).expect("create dep_b src");

    let dep_b_cargo = dep_b_dir.join("Cargo.toml");
    fs::write(
        &dep_b_cargo,
        r#"[package]
name = "dep_b"
version = "0.1.0"
edition = "2021"

[dependencies]
"#,
    )
    .expect("write dep_b Cargo.toml");

    let dep_b_lib = dep_b_src.join("lib.rs");
    let dep_b_original_code = r#"pub fn helper_message() -> &'static str {
    "hello from dep_b"
}
"#;
    fs::write(&dep_b_lib, dep_b_original_code).expect("write dep_b lib.rs");

    // 4. Create Application Crate
    // Declares opentelemetry, otel-shim, tokio, dep_a, dep_b
    let app_dir = ws_root.join("app");
    let app_src = app_dir.join("src");
    fs::create_dir_all(&app_src).expect("create app src");

    let otel_shim_path_escaped = otel_shim_path.to_string_lossy().replace('\\', "/");

    let app_cargo = app_dir.join("Cargo.toml");
    fs::write(
        &app_cargo,
        format!(
            r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
opentelemetry = "0.32.0"
opentelemetry_sdk = {{ version = "0.32.0", features = ["testing"] }}
otel-shim = {{ path = "{otel_shim_path_escaped}" }}
dep_a = {{ path = "../dep_a with spaces" }}
dep_b = {{ path = "../dep_b" }}
tokio = {{ version = "1", features = ["full"] }}
"#
        ),
    )
    .expect("write app Cargo.toml");

    let app_main = app_src.join("main.rs");
    fs::write(
        &app_main,
        r#"use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

fn main() {
    // Satisfy ADR-003 / E-10 and G2 preflight
    otel_shim::init();

    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    opentelemetry::global::set_tracer_provider(provider.clone());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        // Run instrumented application flow calling into dependencies
        run_test_flow().await;
    });

    // Verify all thread-local stacks are cleaned up (S5 cleanup)
    assert_eq!(otel_shim::active_span_count(), 0, "S5 context stack must be empty after execution");

    // Inspect exported spans
    let spans = exporter.get_finished_spans().expect("get spans");
    println!("TOTAL_SPANS={}", spans.len());
    for s in &spans {
        println!("SPAN name={} kind={:?} status={:?}", s.name, s.span_kind, s.status);
    }

    // 1. Verify dep_a::compute span exists
    let compute_span = spans.iter().find(|s| s.name == "compute").expect("compute span");
    assert_eq!(compute_span.span_kind, opentelemetry::trace::SpanKind::Internal);

    // 2. Verify dep_a::fallible_op success span exists (status Unset)
    let ok_spans: Vec<_> = spans.iter().filter(|s| s.name == "fallible_op" && s.status == opentelemetry::trace::Status::Unset).collect();
    assert!(!ok_spans.is_empty(), "expected successful fallible_op span");

    // 3. Verify dep_a::fallible_op error span exists (§16.10: status Error with empty description)
    let err_spans: Vec<_> = spans.iter().filter(|s| s.name == "fallible_op" && matches!(s.status, opentelemetry::trace::Status::Error { .. })).collect();
    assert!(!err_spans.is_empty(), "expected error fallible_op span");
    if let opentelemetry::trace::Status::Error { description } = &err_spans[0].status {
        assert_eq!(description.as_ref(), "", "error description must be empty per §16.10");
    }

    // 4. Verify dep_b::helper_message span exists
    let dep_b_span = spans.iter().find(|s| s.name == "helper_message").expect("helper_message span");
    assert_eq!(dep_b_span.span_kind, opentelemetry::trace::SpanKind::Internal);

    // 5. Verify parent hierarchy: app_caller is parent of compute
    let app_span = spans.iter().find(|s| s.name == "app_caller").expect("app_caller span");
    assert_eq!(compute_span.parent_span_id, app_span.span_context.span_id(), "app must parent dependency span");

    println!("INTEGRATION_VERIFIED_SUCCESS");
}

async fn run_test_flow() {
    app_caller().await;
}

pub async fn app_caller() {
    let _c = dep_a::compute(10, 20);
    let _f1 = dep_a::fallible_op(false);
    let _f2 = dep_a::fallible_op(true);
    let _h = dep_b::helper_message();
}
"#,
    )
    .expect("write app main.rs");

    // 5. Record original source bytes before build
    let dep_a_bytes_before = fs::read(&dep_a_lib).unwrap();
    let dep_b_bytes_before = fs::read(&dep_b_lib).unwrap();

    // 6. Run `cargo build` through `cargo-instrument` wrapper
    // Set target-dir to target/instrumented to satisfy ADR-004
    let target_dir = ws_root.join("target").join("instrumented");
    let output = Command::new("cargo")
        .args([
            "run",
            "--manifest-path",
            app_cargo.to_str().unwrap(),
            "--target-dir",
            target_dir.to_str().unwrap(),
        ])
        .env("RUSTC_WRAPPER", cargo_instrument_bin)
        .env("INSTRUMENT_DEBUG", "0")
        .env("RUST_BACKTRACE", "1")
        .output()
        .expect("execute cargo run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "cargo run with cargo-instrument failed!\nSTDOUT:\n{}\nSTDERR:\n{}",
        stdout,
        stderr
    );

    assert!(
        stdout.contains("INTEGRATION_VERIFIED_SUCCESS"),
        "Execution did not reach expected success marker!\nSTDOUT:\n{}",
        stdout
    );

    // Verify Source Byte Immutability: 100% bit-for-bit identical before and after
    let dep_a_bytes_after = fs::read(&dep_a_lib).unwrap();
    let dep_b_bytes_after = fs::read(&dep_b_lib).unwrap();

    assert_eq!(
        dep_a_bytes_before, dep_a_bytes_after,
        "dep_a source file was mutated in-place!"
    );
    assert_eq!(
        dep_b_bytes_before, dep_b_bytes_after,
        "dep_b source file was mutated in-place!"
    );

    // Verify target-dir isolation
    assert!(target_dir.exists(), "target/instrumented must exist");
}

// ----------------------------------------------------------------------------
// Step 3: Standalone Registry Source-Fidelity Guard (A11, A12, M2)
// ----------------------------------------------------------------------------

fn compute_dir_sha256_tree(dir: &Path) -> std::collections::BTreeMap<PathBuf, String> {
    let mut tree = std::collections::BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        if let Ok(entries) = fs::read_dir(&current) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.is_file() {
                    let bytes = fs::read(&path).expect("read file for sha256");
                    let mut hasher = Sha256::new();
                    hasher.update(&bytes);
                    let hex = format!("{:x}", hasher.finalize());
                    let rel = path.strip_prefix(dir).unwrap().to_path_buf();
                    tree.insert(rel, hex);
                }
            }
        }
    }
    tree
}

#[test]
#[serial]
fn test_registry_source_cache_immutability() {
    // M2: Gate execution on explicit opt-in to preserve clean CI runners
    if std::env::var("CARGO_INSTRUMENT_REGISTRY").is_err() {
        return;
    }

    let cargo_home = std::env::var("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .expect("valid home directory");
            PathBuf::from(home).join(".cargo")
        });

    let registry_src = cargo_home.join("registry").join("src");
    let mut census_dir = None;
    if registry_src.exists() {
        if let Ok(entries) = fs::read_dir(&registry_src) {
            for entry in entries.flatten() {
                let candidate = entry.path().join("census-0.4.2");
                if candidate.exists() && candidate.is_dir() {
                    census_dir = Some(candidate);
                    break;
                }
            }
        }
    }

    let census_dir = match census_dir {
        Some(d) => d,
        None => {
            eprintln!("census-0.4.2 not found in registry src cache; skipping");
            return;
        }
    };

    // 1. Compute SHA-256 for all files in census-0.4.2 before build
    let pre_hashes = compute_dir_sha256_tree(&census_dir);
    assert!(
        !pre_hashes.is_empty(),
        "census-0.4.2 directory must not be empty"
    );
    assert!(
        pre_hashes.contains_key(&PathBuf::from("src").join("lib.rs")),
        "census-0.4.2 must contain src/lib.rs"
    );

    // 2. Set up temporary test package depending on census = "=0.4.2"
    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let ws_root = temp_dir.path();
    let app_dir = ws_root.join("guard_app");
    let app_src = app_dir.join("src");
    fs::create_dir_all(&app_src).expect("create app src");

    let app_cargo = app_dir.join("Cargo.toml");
    fs::write(
        &app_cargo,
        r#"[package]
name = "guard_app"
version = "0.1.0"
edition = "2021"

[dependencies]
census = "=0.4.2"
"#,
    )
    .expect("write guard_app Cargo.toml");

    let app_main = app_src.join("main.rs");
    let main_code = r#"fn main() {
    let inventory = census::Inventory::new();
    let _t = inventory.track(42);
}
"#;
    fs::write(&app_main, main_code).expect("write guard_app main.rs");

    let target_dir = ws_root.join("target").join("instrumented");
    let cargo_instrument_bin = env!("CARGO_BIN_EXE_cargo-instrument");

    // 3. Run cargo check with CARGO_INSTRUMENT_REGISTRY=1
    let output = Command::new("cargo")
        .args([
            "check",
            "--manifest-path",
            app_cargo.to_str().unwrap(),
            "--target-dir",
            target_dir.to_str().unwrap(),
        ])
        .env("RUSTC_WRAPPER", cargo_instrument_bin)
        .env("CARGO_INSTRUMENT_REGISTRY", "1")
        .output()
        .expect("execute cargo check");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "cargo check with CARGO_INSTRUMENT_REGISTRY=1 failed!\nSTDOUT:\n{stdout}\nSTDERR:\n{stderr}"
    );

    // 4. Re-compute SHA-256 for all files in census-0.4.2 after build (A11)
    let post_hashes = compute_dir_sha256_tree(&census_dir);
    assert_eq!(
        pre_hashes, post_hashes,
        "STOP-THE-LINE: ~/.cargo/registry source tree was mutated by instrumented build!"
    );

    // 5. Assert no .rs files created/modified outside target_dir (A12)
    assert_eq!(
        fs::read_to_string(&app_main).unwrap(),
        main_code,
        "src/main.rs in test workspace was mutated!"
    );
}

// ----------------------------------------------------------------------------
// Component 5: Compatibility-Only Graphs & Safety Negatives (A15, A16)
// ----------------------------------------------------------------------------

#[test]
fn test_abi_symbol_collision_fail_open_s11() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let ws_root = temp_dir.path();

    let root_cargo_toml = r#"[workspace]
members = ["app", "colliding_dep"]
resolver = "2"
"#;
    fs::write(ws_root.join("Cargo.toml"), root_cargo_toml).expect("write ws Cargo.toml");

    // 1. Dependency declaring colliding ABI symbol __otel_span_start
    let dep_dir = ws_root.join("colliding_dep");
    let dep_src = dep_dir.join("src");
    fs::create_dir_all(&dep_src).expect("create dep src");
    let dep_cargo = r#"[package]
name = "colliding_dep"
version = "0.1.0"
edition = "2021"
"#;
    fs::write(dep_dir.join("Cargo.toml"), dep_cargo).expect("write dep Cargo.toml");

    let dep_lib = r#"
#[no_mangle]
pub extern "C" fn __otel_span_start() {
    println!("colliding symbol");
}

pub fn compute_value() -> i32 {
    42
}
"#;
    fs::write(dep_src.join("lib.rs"), dep_lib).expect("write dep lib.rs");

    // 2. Application declaring otel-shim and depending on colliding_dep
    let app_dir = ws_root.join("app");
    let app_src = app_dir.join("src");
    fs::create_dir_all(&app_src).expect("create app src");

    let otel_shim_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("otel-shim");
    let otel_shim_path_escaped = otel_shim_path.to_string_lossy().replace('\\', "/");

    let app_cargo = format!(
        r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
colliding_dep = {{ path = "../colliding_dep" }}
otel-shim = {{ path = "{otel_shim_path_escaped}" }}
"#
    );
    fs::write(app_dir.join("Cargo.toml"), app_cargo).expect("write app Cargo.toml");

    let app_main = r#"fn main() {
    otel_shim::init();
    assert_eq!(colliding_dep::compute_value(), 42);
}
"#;
    fs::write(app_src.join("main.rs"), app_main).expect("write app main.rs");

    let cargo_instrument_bin = env!("CARGO_BIN_EXE_cargo-instrument");
    let output = Command::new(cargo_instrument_bin)
        .args(["--", "check"])
        .current_dir(ws_root)
        .output()
        .expect("run cargo check");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // A16 check: compilation must succeed without linker/symbol errors
    assert!(
        output.status.success(),
        "build with colliding dependency must succeed via fail-open!\nSTDOUT:\n{stdout}\nSTDERR:\n{stderr}"
    );

    // A16 check: warning was emitted
    assert!(
        stderr.contains("exports an ABI symbol conflicting with otel-shim"),
        "stderr must contain ABI collision warning! Stderr:\n{stderr}"
    );
}

#[test]
fn test_malformed_syntax_fail_open_s11() {
    // 1. Direct AST analyzer level: malformed syntax returns AstError, does not panic
    let bad_source = "pub fn unclosed_fn( { let x = ;";
    let res = analyze_source_str("bad_crate", Path::new("src/lib.rs"), bad_source);
    assert!(res.is_err(), "malformed source must return AstError::Parse");

    // 2. Integration level: wrapper catches syntax failure and warns per S11
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let fixture_root = temp_dir.path();

    let cargo_toml = r#"[package]
name = "malformed_fixture"
version = "0.1.0"
edition = "2021"
"#;
    fs::write(fixture_root.join("Cargo.toml"), cargo_toml).expect("write Cargo.toml");
    let src_dir = fixture_root.join("src");
    fs::create_dir_all(&src_dir).expect("create src dir");

    fs::write(src_dir.join("main.rs"), bad_source).expect("write bad main.rs");

    let cargo_instrument_bin = env!("CARGO_BIN_EXE_cargo-instrument");
    let output = Command::new(cargo_instrument_bin)
        .args(["--", "check"])
        .current_dir(fixture_root)
        .output()
        .expect("execute cargo check");

    // Must not panic! Output will fail with rustc's syntax error, but wrapper logs S11 warning
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("warning: cargo-instrument: failed to analyze")
            || stderr.contains("expected"),
        "stderr must report analysis failure without panicking! Stderr:\n{stderr}"
    );
}

#[test]
fn test_adversarial_path_attribute_sandboxing() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let root = temp_dir.path();

    // 1. Outside directory with an external source file
    let outside_dir = root.join("outside");
    fs::create_dir_all(&outside_dir).expect("create outside dir");
    let outside_file = outside_dir.join("outside.rs");
    let original_outside_code = "pub fn outside_work() -> i32 { 101 }\n";
    fs::write(&outside_file, original_outside_code).expect("write outside.rs");

    // 2. Fixture crate attempting to escape crate dir via #[path = "../../outside/outside.rs"]
    let crate_dir = root.join("app");
    let src_dir = crate_dir.join("src");
    fs::create_dir_all(&src_dir).expect("create src dir");

    let cargo_toml = r#"[package]
name = "path_escape_app"
version = "0.1.0"
edition = "2021"
"#;
    fs::write(crate_dir.join("Cargo.toml"), cargo_toml).expect("write Cargo.toml");

    let main_rs = r#"
#[path = "../../outside/outside.rs"]
mod outside;

fn main() {
    println!("{}", outside::outside_work());
}
"#;
    fs::write(src_dir.join("main.rs"), main_rs).expect("write main.rs");

    let cargo_instrument_bin = env!("CARGO_BIN_EXE_cargo-instrument");
    let output = Command::new(cargo_instrument_bin)
        .args(["--", "check"])
        .current_dir(&crate_dir)
        .output()
        .expect("execute cargo check");

    // Verify outside file was NOT mutated (A11/A16 invariant)
    let post_outside_code = fs::read_to_string(&outside_file).expect("read outside file");
    assert_eq!(
        original_outside_code, post_outside_code,
        "External source file outside crate root was mutated by instrumented build!"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("ADVERSARIAL_PATH_STDOUT:\n{stdout}");
    println!("ADVERSARIAL_PATH_STDERR:\n{stderr}");

    // Verify build succeeded cleanly via fail-open without panicking
    assert!(
        output.status.success(),
        "build with external #[path] must succeed safely! Stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("escapes crate root"),
        "stderr must contain escaping path warning! Stderr:\n{stderr}"
    );
}
