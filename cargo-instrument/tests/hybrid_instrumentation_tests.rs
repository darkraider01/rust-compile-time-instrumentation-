use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cargo_instrument::ast::analyze_source_str;
use opentelemetry::trace::TraceContextExt as _;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Registry;

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

fn describe(output: &Output) -> String {
    format!(
        "exit: {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

fn run_cargo(manifest_dir: &Path, target_dir: &Path, args: &[&str], instrumented: bool) -> Output {
    let mut cmd = Command::new("cargo");
    if let Some((subcmd, rest)) = args.split_first() {
        cmd.arg(subcmd)
            .arg("--target-dir")
            .arg(target_dir)
            .args(rest);
    } else {
        cmd.arg("--target-dir").arg(target_dir);
    }
    cmd.current_dir(manifest_dir)
        .env("CARGO_TERM_COLOR", "never");

    if instrumented {
        cmd.env("RUSTC_WRAPPER", env!("CARGO_BIN_EXE_cargo-instrument"))
            .env("INSTRUMENT_DEBUG", "1");
    }

    cmd.output().expect("failed to execute cargo")
}

// ----------------------------------------------------------------------------
// Unit verification: tracing-opentelemetry context activation & AST skipping
// ----------------------------------------------------------------------------

#[tracing::instrument]
fn sample_instrumented_fn() -> (bool, opentelemetry::trace::SpanId) {
    let cx = opentelemetry::Context::current();
    let has_active = cx.has_active_span();
    let span_id = cx.span().span_context().span_id();
    (has_active, span_id)
}

#[test]
fn test_tracing_opentelemetry_activates_context_by_default() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let tracer = opentelemetry::trace::TracerProvider::tracer(&provider, "test_tracer");
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
    let subscriber = Registry::default().with(otel_layer);

    tracing::subscriber::with_default(subscriber, || {
        let (has_active, span_id) = sample_instrumented_fn();
        assert!(
            has_active,
            "OpenTelemetryLayer::with_context_activation must be enabled by default"
        );
        assert_ne!(
            span_id,
            opentelemetry::trace::SpanId::INVALID,
            "Active span ID must be valid"
        );
    });

    let spans = exporter.get_finished_spans().expect("get spans");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "sample_instrumented_fn");
}

#[test]
fn test_ast_coexistence_attribute_and_body_exclusion() {
    let code = r#"
#[tracing::instrument]
pub fn explicit_tracing_fn(a: i32) -> i32 {
    a + 1
}

#[propagate_context]
pub fn explicit_propagate_fn(b: i32) -> i32 {
    let __otel_cx = opentelemetry::Context::current();
    let _ = &__otel_cx;
    b + 2
}

pub fn explicit_body_with_context(c: i32) -> i32 {
    let cx = opentelemetry::Context::current();
    let _ = c.with_context(cx);
    c + 3
}

pub fn unannotated_target(d: i32) -> i32 {
    d * 4
}
"#;

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code)
        .expect("analysis should succeed");

    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].function_name, "unannotated_target");
    assert_eq!(report.skipped_stats.handwritten_otel, 3);
    assert_eq!(
        report.candidates.len() + report.skipped_stats.total(),
        4,
        "Reconciliation invariant must hold exactly"
    );
}

// ----------------------------------------------------------------------------
// Item 2: Collision hazard regression guard under #![deny(warnings)]
// ----------------------------------------------------------------------------

#[test]
fn test_propagate_context_collision_hazard_prevented() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    let target_dir = root.join("target");

    // 1. Proc-macro providing dummy #[propagate_context] attribute
    write_file(
        &root.join("dummy_macro").join("Cargo.toml"),
        r#"
[package]
name = "dummy_macro"
version = "0.1.0"
edition = "2021"

[workspace]

[lib]
proc-macro = true
"#,
    );
    write_file(
        &root.join("dummy_macro").join("src").join("lib.rs"),
        r#"
extern crate proc_macro;
use proc_macro::TokenStream;

#[proc_macro_attribute]
pub fn propagate_context(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}
"#,
    );

    // 2. Fixture crate compiling under #![deny(warnings)] with simulated macro output
    let otel_shim_path = otel_shim_dep_path();
    write_file(
        &root.join("collision_crate").join("Cargo.toml"),
        &format!(
            r#"
[package]
name = "collision_crate"
version = "0.1.0"
edition = "2021"

[workspace]

[dependencies]
dummy_macro = {{ path = "../dummy_macro" }}
opentelemetry = "0.32.0"
otel-shim = {{ path = "{otel_shim_path}" }}
"#
        ),
    );

    write_file(
        &root.join("collision_crate").join("src").join("lib.rs"),
        r#"
#![deny(warnings)]

use dummy_macro::propagate_context;

// References otel_shim to pass preflight check (ADR-003 / E-10)
pub fn init() {
    let _ = otel_shim::active_span_count();
}

// Simulated #[propagate_context] with internal __otel_cx binding
#[propagate_context]
pub fn function_with_propagate_context(x: i32) -> i32 {
    let __otel_cx = opentelemetry::Context::current();
    let _ = &__otel_cx;
    x + 1
}

// Function with manual context propagation call (.with_context)
pub fn function_with_body_context(x: i32) -> i32 {
    let cx = opentelemetry::Context::current();
    let _ = helper_with_context(x, cx);
    x + 2
}

fn helper_with_context(x: i32, _cx: opentelemetry::Context) -> i32 {
    x
}

// Unannotated function that must be instrumented cleanly
pub fn unannotated_clean(x: i32) -> i32 {
    x * 3
}
"#,
    );

    // Run cargo check with RUSTC_WRAPPER=cargo-instrument
    let output = run_cargo(
        &root.join("collision_crate"),
        &target_dir,
        &["check"],
        true,
    );
    assert!(
        output.status.success(),
        "cargo check under #![deny(warnings)] failed with cargo-instrument wrapper!\n{}",
        describe(&output)
    );

    // Inspect mirrored files: verify unannotated_clean is instrumented,
    // while function_with_propagate_context and function_with_body_context are NOT.
    let mirror_dir = target_dir
        .join("debug")
        .join("deps")
        .join("instrumented_sources");
    
    let mut mirrored_lib_paths = Vec::new();
    if let Ok(entries) = fs::read_dir(&mirror_dir) {
        for entry in entries.flatten() {
            let path = entry.path().join("src").join("lib.rs");
            if path.exists() {
                mirrored_lib_paths.push(path);
            }
        }
    }

    assert!(
        !mirrored_lib_paths.is_empty(),
        "Expected at least one mirrored source file in {:?}",
        mirror_dir
    );

    let mirrored_content = fs::read_to_string(&mirrored_lib_paths[0])
        .expect("read mirrored lib.rs");

    assert!(
        mirrored_content.contains("/* __cargo_instrument_anchor: \"unannotated_clean\" */"),
        "Unannotated function must be instrumented"
    );
    assert!(
        !mirrored_content.contains("/* __cargo_instrument_anchor: \"function_with_propagate_context\" */"),
        "#[propagate_context] function must NOT be instrumented"
    );
    assert!(
        !mirrored_content.contains("/* __cargo_instrument_anchor: \"function_with_body_context\" */"),
        "Body with_context function must NOT be instrumented"
    );
}

// ----------------------------------------------------------------------------
// Item 3: Hybrid tracing caller -> automatic dependency parenting integration tests
// ----------------------------------------------------------------------------

#[test]
fn test_hybrid_tracing_caller_otel_dependency_parenting() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    let target_dir = root.join("target");
    let otel_shim_path = otel_shim_dep_path();

    // 1. Dependency crate `dep_lib` (automatically instrumented via Tier-2 C ABI)
    write_file(
        &root.join("dep_lib").join("Cargo.toml"),
        r#"
[package]
name = "dep_lib"
version = "0.1.0"
edition = "2021"

[workspace]
"#,
    );
    write_file(
        &root.join("dep_lib").join("src").join("lib.rs"),
        r#"
pub fn calculate_sum(a: i32, b: i32) -> i32 {
    a + b
}
"#,
    );

    // 2. Application crate `hybrid_app` with tracing + tracing-opentelemetry + async-trait
    write_file(
        &root.join("hybrid_app").join("Cargo.toml"),
        &format!(
            r#"
[package]
name = "hybrid_app"
version = "0.1.0"
edition = "2021"

[workspace]

[dependencies]
dep_lib = {{ path = "../dep_lib" }}
otel-shim = {{ path = "{otel_shim_path}" }}
opentelemetry = "0.32.0"
opentelemetry_sdk = {{ version = "0.32.0", features = ["testing"] }}
tracing = "0.1"
tracing-subscriber = {{ version = "0.3", features = ["registry"] }}
tracing-opentelemetry = "0.33"
async-trait = "0.1"
tokio = {{ version = "1", features = ["macros", "rt-multi-thread", "time"] }}
"#
        ),
    );

    write_file(
        &root.join("hybrid_app").join("src").join("lib.rs"),
        r#"
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Registry;

pub fn init_telemetry() -> InMemorySpanExporter {
    // Reference otel_shim to prevent extern crate pruning per ADR-003 / E-10
    otel_shim::init();

    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let tracer = opentelemetry::trace::TracerProvider::tracer(&provider, "hybrid_app");
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
    let subscriber = Registry::default().with(otel_layer);
    let _ = tracing::subscriber::set_global_default(subscriber);
    let _ = opentelemetry::global::set_tracer_provider(provider);
    exporter
}

#[tracing::instrument]
pub fn caller_sync_operation(x: i32, y: i32) -> i32 {
    // Calling automatically instrumented dependency
    dep_lib::calculate_sum(x, y)
}

#[async_trait::async_trait]
pub trait AsyncCalculator {
    async fn compute(&self, a: i32, b: i32) -> i32;
}

pub struct CalculatorService;

#[async_trait::async_trait]
impl AsyncCalculator for CalculatorService {
    #[tracing::instrument(skip(self))]
    async fn compute(&self, a: i32, b: i32) -> i32 {
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        dep_lib::calculate_sum(a, b)
    }
}
"#,
    );

    // 3. Tests in `hybrid_app`
    write_file(
        &root.join("hybrid_app").join("tests").join("hybrid_suite.rs"),
        r#"
use hybrid_app::*;

#[test]
fn test_sync_hybrid_parenting() {
    let exporter = init_telemetry();

    let res = caller_sync_operation(10, 20);
    assert_eq!(res, 30);

    let spans = exporter.get_finished_spans().expect("get spans");
    
    let caller_span = spans
        .iter()
        .find(|s| s.name == "caller_sync_operation")
        .expect("find caller_sync_operation span");
    let dep_span = spans
        .iter()
        .find(|s| s.name == "calculate_sum" && s.parent_span_id == caller_span.span_context.span_id())
        .expect("find calculate_sum span parented under caller_sync_operation");

    // 1. Dependency span parent_span_id matches caller's span_id
    assert_eq!(
        dep_span.parent_span_id,
        caller_span.span_context.span_id(),
        "Dependency span must parent under caller #[tracing::instrument] span"
    );

    // 2. Exactly 1 span for #[tracing::instrument]
    let caller_spans: Vec<_> = spans
        .iter()
        .filter(|s| s.name == "caller_sync_operation")
        .collect();
    assert_eq!(caller_spans.len(), 1, "Exactly one caller span expected");

    // 3. otel_shim active_span_count must be 0
    assert_eq!(
        otel_shim::active_span_count(),
        0,
        "Active span count must cleanly return to 0"
    );
}
"#,
    );

    // Run cargo test against hybrid_app with cargo-instrument
    let output = run_cargo(
        &root.join("hybrid_app"),
        &target_dir,
        &["test", "--test", "hybrid_suite", "--", "--nocapture"],
        true,
    );

    assert!(
        output.status.success(),
        "hybrid_suite tests failed under cargo-instrument!\n{}",
        describe(&output)
    );
}

#[test]
fn test_async_trait_hybrid_parenting() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    let target_dir = root.join("target");
    let otel_shim_path = otel_shim_dep_path();

    // 1. Dependency crate `dep_lib` (automatically instrumented via Tier-2 C ABI)
    write_file(
        &root.join("dep_lib").join("Cargo.toml"),
        r#"
[package]
name = "dep_lib"
version = "0.1.0"
edition = "2021"

[workspace]
"#,
    );
    write_file(
        &root.join("dep_lib").join("src").join("lib.rs"),
        r#"
pub fn calculate_sum(a: i32, b: i32) -> i32 {
    a + b
}
"#,
    );

    // 2. Application crate `async_hybrid_app`
    write_file(
        &root.join("async_hybrid_app").join("Cargo.toml"),
        &format!(
            r#"
[package]
name = "async_hybrid_app"
version = "0.1.0"
edition = "2021"

[workspace]

[dependencies]
dep_lib = {{ path = "../dep_lib" }}
otel-shim = {{ path = "{otel_shim_path}" }}
opentelemetry = "0.32.0"
opentelemetry_sdk = {{ version = "0.32.0", features = ["testing"] }}
tracing = "0.1"
tracing-subscriber = {{ version = "0.3", features = ["registry"] }}
tracing-opentelemetry = "0.33"
async-trait = "0.1"
tokio = {{ version = "1", features = ["macros", "rt-multi-thread", "time"] }}
"#
        ),
    );

    write_file(
        &root.join("async_hybrid_app").join("src").join("lib.rs"),
        r#"
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Registry;

pub fn init_telemetry() -> InMemorySpanExporter {
    otel_shim::init();

    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let tracer = opentelemetry::trace::TracerProvider::tracer(&provider, "async_hybrid_app");
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
    let subscriber = Registry::default().with(otel_layer);
    let _ = tracing::subscriber::set_global_default(subscriber);
    let _ = opentelemetry::global::set_tracer_provider(provider);
    exporter
}

#[async_trait::async_trait]
pub trait AsyncCalculator {
    async fn compute(&self, a: i32, b: i32) -> i32;
}

pub struct CalculatorService;

#[async_trait::async_trait]
impl AsyncCalculator for CalculatorService {
    #[tracing::instrument(skip(self))]
    async fn compute(&self, a: i32, b: i32) -> i32 {
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        dep_lib::calculate_sum(a, b)
    }
}
"#,
    );

    // 3. Test in `async_hybrid_app`
    write_file(
        &root.join("async_hybrid_app").join("tests").join("async_suite.rs"),
        r#"
use async_hybrid_app::*;

#[tokio::test]
async fn test_async_parenting() {
    let exporter = init_telemetry();
    let service = CalculatorService;

    let res = service.compute(40, 50).await;
    assert_eq!(res, 90);

    let spans = exporter.get_finished_spans().expect("get spans");

    let caller_span = spans
        .iter()
        .find(|s| s.name == "compute")
        .expect("find compute span");
    let dep_span = spans
        .iter()
        .find(|s| s.name == "calculate_sum" && s.parent_span_id == caller_span.span_context.span_id())
        .expect("find calculate_sum span parented under compute");

    // Parenting assertion across async_trait boundary
    assert_eq!(
        dep_span.parent_span_id,
        caller_span.span_context.span_id(),
        "Dependency span must parent under async_trait caller span"
    );

    // Exactly 1 span for compute
    assert_eq!(
        spans.iter().filter(|s| s.name == "compute").count(),
        1
    );

    // Active span count must cleanly return to 0
    assert_eq!(
        otel_shim::active_span_count(),
        0,
        "Active span count must cleanly return to 0 after async_trait call"
    );
}
"#,
    );

    // Run cargo test against async_hybrid_app with cargo-instrument
    let output = run_cargo(
        &root.join("async_hybrid_app"),
        &target_dir,
        &["test", "--test", "async_suite", "--", "--nocapture"],
        true,
    );

    assert!(
        output.status.success(),
        "async_suite tests failed under cargo-instrument!\n{}",
        describe(&output)
    );
}
