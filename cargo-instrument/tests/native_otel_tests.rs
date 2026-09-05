use std::fs;
use std::path::Path;
use std::process::Command;

use cargo_instrument::ast::analyze_source_str;
use cargo_instrument::discovery::CrateInvocation;
use cargo_instrument::transform::{
    transform_source_str_with_native_otel, Emitter, InstrumentationIntent, NativeOtelEmitter,
    SkipReason, SpanKind, TransformationPlan,
};

#[test]
fn test_native_otel_ordinary_sync_function() {
    let source = r#"
fn compute(a: i32, b: i32) -> i32 {
    let sum = a + b;
    sum * 2
}
"#;
    let report =
        analyze_source_str("my_crate", Path::new("src/lib.rs"), source).expect("analyze source");
    assert_eq!(report.candidates.len(), 1);
    assert!(!report.candidates[0].returns_result);

    let transformed = transform_source_str_with_native_otel(source, "my_crate", &report.candidates)
        .expect("transform source");

    // Must contain OpenTelemetry span builder and attach guard
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"compute\" */"));
    assert!(
        transformed.contains("let __otel_tracer = opentelemetry::global::tracer(\"my_crate\");")
    );
    assert!(transformed.contains(
        "let __otel_span = opentelemetry::trace::Tracer::span_builder(&__otel_tracer, \"compute\")"
    ));
    assert!(transformed.contains(".with_kind(opentelemetry::trace::SpanKind::Internal)"));
    assert!(transformed.contains(".start(&__otel_tracer);"));
    assert!(transformed.contains("let __otel_cx = <opentelemetry::Context as opentelemetry::trace::TraceContextExt>::current_with_span(__otel_span);"));
    assert!(transformed.contains("let __otel_guard = __otel_cx.attach();"));

    // Non-Result function must NOT contain closure wrapping
    assert!(!transformed.contains("__otel_res"));
    assert!(!transformed.contains("redundant_closure_call"));

    // Original body must remain untouched inside the block
    assert!(transformed.contains("let sum = a + b;"));
    assert!(transformed.contains("sum * 2"));
}

#[test]
fn test_native_otel_inherent_and_trait_methods() {
    let source = r#"
struct Worker;

impl Worker {
    pub fn process(&self, count: usize) -> usize {
        count * 10
    }
}

trait Action {
    fn execute(&self);
}

impl Action for Worker {
    fn execute(&self) {
        let _ = 1;
    }
}
"#;
    let report =
        analyze_source_str("service", Path::new("src/lib.rs"), source).expect("analyze source");
    assert_eq!(report.candidates.len(), 2);

    let transformed = transform_source_str_with_native_otel(source, "service", &report.candidates)
        .expect("transform source");

    assert!(transformed.contains("/* __cargo_instrument_anchor: \"Worker::process\" */"));
    assert!(
        transformed.contains("/* __cargo_instrument_anchor: \"<Worker as Action>::execute\" */")
    );
    assert!(transformed.contains("let __otel_tracer = opentelemetry::global::tracer(\"service\");"));
}

#[test]
fn test_native_otel_generic_and_unsafe_functions() {
    let source = r#"
fn identity<T: Clone>(item: T) -> T {
    item.clone()
}

unsafe fn raw_read(ptr: *const i32) -> i32 {
    *ptr
}
"#;
    let report =
        analyze_source_str("core", Path::new("src/lib.rs"), source).expect("analyze source");
    assert_eq!(report.candidates.len(), 2);

    let transformed = transform_source_str_with_native_otel(source, "core", &report.candidates)
        .expect("transform source");

    assert!(transformed.contains("/* __cargo_instrument_anchor: \"identity\" */"));
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"raw_read\" */"));
    assert!(!transformed.contains("__otel_res"));
}

#[test]
fn test_native_otel_result_returning_function_shape() {
    let source = r#"
fn fetch_data(id: u64) -> Result<String, std::io::Error> {
    if id == 0 {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "zero"));
    }
    Ok("found".to_string())
}
"#;
    let report =
        analyze_source_str("api", Path::new("src/lib.rs"), source).expect("analyze source");
    assert_eq!(report.candidates.len(), 1);
    assert!(report.candidates[0].returns_result);

    let transformed = transform_source_str_with_native_otel(source, "api", &report.candidates)
        .expect("transform source");

    // Result function MUST use clone().attach()
    assert!(transformed.contains("let __otel_guard = __otel_cx.clone().attach();"));
    // Result function MUST use #[allow(clippy::redundant_closure_call)]
    assert!(transformed.contains("#[allow(clippy::redundant_closure_call)]"));
    // Result function MUST use let __otel_res: Result<_, _> = (|| { ... })();
    assert!(transformed.contains("let __otel_res: Result<_, _> = (|| {"));
    assert!(transformed.contains("})();"));
    // Result function MUST set error status on Err
    assert!(transformed.contains("if __otel_res.is_err() {"));
    assert!(transformed.contains("opentelemetry::trace::TraceContextExt::span(&__otel_cx)"));
    assert!(transformed.contains(".set_status(opentelemetry::trace::Status::error(\"\"));"));
    assert!(transformed.contains("__otel_res"));
    // Must NOT contain explicit drop(__otel_guard)
    assert!(!transformed.contains("drop(__otel_guard)"));
}

#[test]
fn test_native_otel_question_mark_and_early_return() {
    let source = r#"
fn parse_number(s: &str) -> Result<i32, std::num::ParseIntError> {
    let val = s.parse::<i32>()?;
    if val < 0 {
        return Ok(0);
    }
    Ok(val)
}
"#;
    let report =
        analyze_source_str("parser", Path::new("src/lib.rs"), source).expect("analyze source");
    assert_eq!(report.candidates.len(), 1);
    assert!(report.candidates[0].returns_result);

    let transformed = transform_source_str_with_native_otel(source, "parser", &report.candidates)
        .expect("transform source");

    assert!(transformed.contains("let __otel_res: Result<_, _> = (|| {"));
    assert!(transformed.contains("let val = s.parse::<i32>()?;"));
    assert!(transformed.contains("return Ok(0);"));
    assert!(transformed.contains("if __otel_res.is_err() {"));
}

#[test]
fn test_native_otel_diverging_function_and_panicking_result() {
    let source = r#"
fn abort() -> ! {
    panic!("fatal crash");
}

fn panicking_result() -> Result<i32, String> {
    panic!("always panics");
}
"#;
    let report = analyze_source_str("diverge_crate", Path::new("src/lib.rs"), source)
        .expect("analyze source");
    assert_eq!(report.candidates.len(), 2);

    let abort_candidate = report
        .candidates
        .iter()
        .find(|c| c.function_name == "abort")
        .unwrap();
    assert!(!abort_candidate.returns_result);

    let panicking_candidate = report
        .candidates
        .iter()
        .find(|c| c.function_name == "panicking_result")
        .unwrap();
    assert!(panicking_candidate.returns_result);

    let transformed =
        transform_source_str_with_native_otel(source, "diverge_crate", &report.candidates)
            .expect("transform source");

    // abort() is non-Result: pure prefix
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"abort\" */"));
    // panicking_result() is Result: wrapped in Result closure with pinned Result<_, _>
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"panicking_result\" */"));
    assert!(transformed.contains("let __otel_res: Result<_, _> = (|| {"));
}

#[test]
fn test_native_otel_tracer_name_and_intent() {
    let source = "fn task() {}\n";
    let report = analyze_source_str("telemetry_app", Path::new("src/lib.rs"), source)
        .expect("analyze source");
    let c = &report.candidates[0];

    let intent = InstrumentationIntent::from_candidate(c, "telemetry_app");
    assert_eq!(intent.span_name, "task");
    assert_eq!(intent.tracer_name, "telemetry_app");
    assert_eq!(intent.span_kind, SpanKind::Internal);
    assert!(!intent.returns_result);

    let emitter = NativeOtelEmitter::new("telemetry_app");
    let prefix = emitter.emit_body_prefix(c, "\n");
    assert!(prefix.contains("opentelemetry::global::tracer(\"telemetry_app\")"));
    assert!(prefix.contains("span_builder(&__otel_tracer, \"task\")"));
}

#[test]
fn test_native_otel_idempotence() {
    let source = r#"
fn simple() {
    println!("hello");
}
"#;
    let report =
        analyze_source_str("test_crate", Path::new("src/lib.rs"), source).expect("analyze source");
    assert_eq!(report.candidates.len(), 1);

    let emitter = NativeOtelEmitter::new("test_crate");
    let plan1 = TransformationPlan::build_with_emitter(source, &report.candidates, &emitter)
        .expect("plan 1");
    assert_eq!(plan1.edits.len(), 1);
    assert_eq!(plan1.skipped.len(), 0);

    let transformed = plan1.apply(source).expect("apply 1");

    // Splicer-level idempotence: applying original cached candidate plan to transformed text skips with AlreadyInstrumented
    let plan2a = TransformationPlan::build_with_emitter(&transformed, &report.candidates, &emitter)
        .expect("plan 2a");
    assert_eq!(
        plan2a.edits.len(),
        0,
        "no edits generated on already instrumented text"
    );
    assert_eq!(plan2a.skipped.len(), 1);
    assert_eq!(plan2a.skipped[0].reason, SkipReason::AlreadyInstrumented);
    assert_eq!(plan2a.apply(&transformed).unwrap(), transformed);

    // AST-level idempotence: re-analyzing transformed source detects OTel calls and excludes them
    let report2 = analyze_source_str("test_crate", Path::new("src/lib.rs"), &transformed)
        .expect("analyze transformed");
    assert_eq!(
        report2.candidates.len(),
        0,
        "AST analysis excludes already instrumented functions"
    );
}

#[test]
fn test_native_otel_marker_false_positive_protection() {
    let source = r#"
fn with_marker_string() {
    let msg = "/* __cargo_instrument_anchor: fake */";
    println!("{msg}");
}
"#;
    let report =
        analyze_source_str("test_crate", Path::new("src/lib.rs"), source).expect("analyze source");
    assert_eq!(report.candidates.len(), 1);

    let emitter = NativeOtelEmitter::new("test_crate");
    let plan = TransformationPlan::build_with_emitter(source, &report.candidates, &emitter)
        .expect("plan should build");

    assert_eq!(
        plan.edits.len(),
        1,
        "anchor inside string literal must not suppress instrumentation"
    );
    assert_eq!(plan.skipped.len(), 0);
}

#[test]
fn test_native_otel_async_deferred_to_p1_6() {
    let source = r#"
pub async fn async_handler() -> String {
    "async result".to_string()
}

pub fn sync_handler() -> String {
    "sync result".to_string()
}
"#;
    let report =
        analyze_source_str("web_app", Path::new("src/lib.rs"), source).expect("analyze source");
    assert_eq!(report.candidates.len(), 2);

    let async_c = report
        .candidates
        .iter()
        .find(|c| c.function_name == "async_handler")
        .unwrap();
    assert!(async_c.is_async);

    let sync_c = report
        .candidates
        .iter()
        .find(|c| c.function_name == "sync_handler")
        .unwrap();
    assert!(!sync_c.is_async);

    let emitter = NativeOtelEmitter::new("web_app");
    assert!(
        !emitter.handles_async(),
        "NativeOtelEmitter must return false for handles_async in P1.5"
    );

    let plan = TransformationPlan::build_with_emitter(source, &report.candidates, &emitter)
        .expect("build plan");

    // Sync candidate accepted, async candidate deferred
    assert_eq!(plan.edits.len(), 1);
    assert_eq!(plan.skipped.len(), 1);
    assert_eq!(plan.skipped[0].function_name, "async_handler");
    assert_eq!(plan.skipped[0].reason, SkipReason::AsyncDeferred);

    let transformed = plan.apply(source).expect("apply plan");
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"sync_handler\" */"));
    assert!(!transformed.contains("/* __cargo_instrument_anchor: \"async_handler\" */"));
}

#[test]
fn test_native_otel_dependency_gate_fail_open() {
    // Discovery classification without opentelemetry
    let args_without_otel = vec![
        "rustc".to_string(),
        "--crate-name".to_string(),
        "pure_app".to_string(),
        "src/main.rs".to_string(),
        "--extern".to_string(),
        "serde=target/deps/libserde.rlib".to_string(),
    ];
    let inv_without = CrateInvocation::parse(&args_without_otel).expect("parse args without otel");
    assert!(!inv_without.unit.has_opentelemetry());

    // Discovery classification with opentelemetry
    let args_with_otel = vec![
        "rustc".to_string(),
        "--crate-name".to_string(),
        "otel_app".to_string(),
        "src/main.rs".to_string(),
        "--extern".to_string(),
        "opentelemetry=target/deps/libopentelemetry.rlib".to_string(),
    ];
    let inv_with = CrateInvocation::parse(&args_with_otel).expect("parse args with otel");
    assert!(inv_with.unit.has_opentelemetry());
}

#[test]
fn test_native_otel_comments_and_formatting_preservation() {
    let source = r#"// 🦀 Leading header comment with UTF-8
/* Multi-line
   block comment */

#[inline]
#[allow(dead_code)]
fn formatted_fn(x: i32) -> i32 {
    // Indented comment inside body
    let doubled = x * 2;
    doubled
}

// Trailing comment
"#;
    let report =
        analyze_source_str("format_test", Path::new("src/lib.rs"), source).expect("analyze source");

    let transformed =
        transform_source_str_with_native_otel(source, "format_test", &report.candidates)
            .expect("transform source");

    assert!(transformed.starts_with("// 🦀 Leading header comment with UTF-8"));
    assert!(transformed.contains("/* Multi-line\n   block comment */"));
    assert!(transformed.contains("#[inline]\n#[allow(dead_code)]"));
    assert!(transformed.contains("// Indented comment inside body"));
    assert!(transformed.ends_with("// Trailing comment\n"));
}

#[test]
fn test_native_otel_live_rustc_and_clippy_compilation_proof() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let project_root = temp_dir.path().join("proof_crate");
    let src_dir = project_root.join("src");
    fs::create_dir_all(&src_dir).expect("create src dir");

    let cargo_toml = r#"
[package]
name = "proof_crate"
version = "0.1.0"
edition = "2021"

[dependencies]
opentelemetry = "0.32.0"
"#;
    fs::write(project_root.join("Cargo.toml"), cargo_toml).expect("write Cargo.toml");

    // All 8 core function shapes in a single source file
    let source = r#"#![deny(warnings)]

// 1. Diverging body in Result
pub fn always_panics() -> Result<i32, String> {
    panic!("never returns");
}

// 2. impl Trait in Result
pub fn iter_items() -> Result<impl Iterator<Item = i32>, String> {
    Ok(vec![1, 2, 3].into_iter())
}

// 3. Generic with where clause and ?
pub fn generic_parse<T: std::str::FromStr>(s: &str) -> Result<T, T::Err> {
    s.parse()
}

// 4. Borrowed reference in Result
pub struct Service<'a>(pub &'a str);
impl<'a> Service<'a> {
    pub fn get_ref(&self) -> Result<&str, String> {
        Ok(self.0)
    }
}

// 5. Mutable self mutation in Result
pub struct Counter(pub i32);
impl Counter {
    pub fn inc(&mut self) -> Result<i32, String> {
        self.0 += 1;
        Ok(self.0)
    }
}

// 6. Consuming self in Result
pub struct Consumer(pub String);
impl Consumer {
    pub fn consume(self) -> Result<String, String> {
        Ok(self.0)
    }
}

// 7. Explicit early return
pub fn early_return(x: i32) -> Result<i32, &'static str> {
    if x < 0 {
        return Err("negative");
    }
    Ok(x * 2)
}

// 8. Non-Result synchronous function
pub fn non_result(x: i32) -> i32 {
    x + 1
}

// 9. Diverging ! function
pub fn terminates() -> ! {
    panic!("halt");
}

/// # Safety
/// Caller must ensure pointer is valid and properly aligned.
pub unsafe fn raw_deref(p: *const i32) -> i32 {
    *p
}
"#;

    // 1. Analyze candidates
    let report =
        analyze_source_str("proof_crate", Path::new("src/lib.rs"), source).expect("analyze source");
    assert_eq!(report.candidates.len(), 10);

    // 2. Transform with NativeOtelEmitter
    let transformed =
        transform_source_str_with_native_otel(source, "proof_crate", &report.candidates)
            .expect("transform source");

    // Write transformed code to disk
    fs::write(src_dir.join("lib.rs"), &transformed).expect("write transformed lib.rs");

    // 3. Live verification 1: cargo check under #![deny(warnings)] with real opentelemetry 0.32.0
    let check_status = Command::new("cargo")
        .arg("check")
        .current_dir(&project_root)
        .status()
        .expect("execute cargo check");
    assert!(
        check_status.success(),
        "Live cargo check failed on native OpenTelemetry instrumented code!"
    );

    // 4. Live verification 2: cargo clippy under -D warnings
    let clippy_output = Command::new("cargo")
        .arg("clippy")
        .arg("--")
        .arg("-D")
        .arg("warnings")
        .current_dir(&project_root)
        .output()
        .expect("execute cargo clippy");
    assert!(
        clippy_output.status.success(),
        "Live cargo clippy failed on native OpenTelemetry instrumented code! stderr:\n{}",
        String::from_utf8_lossy(&clippy_output.stderr)
    );
}
