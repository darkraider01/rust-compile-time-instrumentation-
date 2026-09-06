use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
