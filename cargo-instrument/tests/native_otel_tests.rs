use std::fs;
use std::path::Path;
use std::process::Command;

use cargo_instrument::ast::analyze_source_str;
use cargo_instrument::discovery::CrateInvocation;
use cargo_instrument::transform::{
    detect_line_ending, transform_source_str_with_native_otel, Emitter, NativeOtelEmitter,
    SkipReason, TransformationPlan,
};
use opentelemetry::trace::Span as _;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

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
fn test_native_otel_tracer_name_and_scope() {
    let source = "fn task() {}\n";
    let report = analyze_source_str("telemetry_app", Path::new("src/lib.rs"), source)
        .expect("analyze source");
    let c = &report.candidates[0];

    let emitter = NativeOtelEmitter::new("telemetry_app");
    assert_eq!(emitter.crate_name, "telemetry_app");

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

    // Emitters that report handles_async() == false still defer async functions with SkipReason::AsyncDeferred
    struct SyncOnlyEmitter;
    impl Emitter for SyncOnlyEmitter {
        fn emit_body_prefix(
            &self,
            candidate: &cargo_instrument::candidate::Candidate,
            nl: &str,
        ) -> String {
            format!("{nl}    /* sync_only: {} */", candidate.function_name)
        }
        fn emit_body_suffix(
            &self,
            _candidate: &cargo_instrument::candidate::Candidate,
            _nl: &str,
        ) -> String {
            String::new()
        }
        fn handles_async(&self) -> bool {
            false
        }
    }

    let emitter = SyncOnlyEmitter;
    assert!(!emitter.handles_async());

    let plan = TransformationPlan::build_with_emitter(source, &report.candidates, &emitter)
        .expect("build plan");

    // Sync candidate accepted, async candidate deferred
    assert_eq!(plan.edits.len(), 1);
    assert_eq!(plan.skipped.len(), 1);
    assert_eq!(plan.skipped[0].function_name, "async_handler");
    assert_eq!(plan.skipped[0].reason, SkipReason::AsyncDeferred);

    let transformed = plan.apply(source).expect("apply plan");
    assert!(transformed.contains("/* sync_only: sync_handler */"));
    assert!(!transformed.contains("/* sync_only: async_handler */"));

    // NativeOtelEmitter handles async in P1.6
    let otel_emitter = NativeOtelEmitter::new("web_app");
    assert!(
        otel_emitter.handles_async(),
        "NativeOtelEmitter must return true for handles_async in P1.6"
    );
    let otel_plan =
        TransformationPlan::build_with_emitter(source, &report.candidates, &otel_emitter)
            .expect("build otel plan");
    assert_eq!(
        otel_plan.edits.len(),
        3,
        "sync candidate has 1 edit (prefix), async candidate has 2 edits (prefix + suffix)"
    );
    assert_eq!(otel_plan.skipped.len(), 0, "no candidates skipped");
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
fn test_native_otel_mut_ref_return_fallback_c1() {
    let source = r#"
pub struct Holder {
    pub name: String,
}

impl Holder {
    pub fn get_mut(&mut self) -> Result<&mut String, String> {
        Ok(&mut self.name)
    }
}

pub fn nth_mut(s: &mut [u8], i: usize) -> Result<&mut u8, String> {
    s.get_mut(i).ok_or_else(|| "out of bounds".to_string())
}
"#;
    let report =
        analyze_source_str("c1_crate", Path::new("src/lib.rs"), source).expect("analyze source");
    assert_eq!(report.candidates.len(), 2);

    // Both candidates return Result AND return mutable references / references
    assert!(report.candidates[0].returns_result);
    assert!(report.candidates[0].returns_mut_reference);
    assert!(report.candidates[0].returns_reference_or_lifetime);
    assert!(report.candidates[1].returns_result);
    assert!(report.candidates[1].returns_mut_reference);
    assert!(report.candidates[1].returns_reference_or_lifetime);

    let transformed = transform_source_str_with_native_otel(source, "c1_crate", &report.candidates)
        .expect("transform source");

    // Both functions MUST contain anchors and span creation
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"Holder::get_mut\" */"));
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"nth_mut\" */"));

    // Crucial C1 invariant: mutable reference returners MUST fall back to prefix-only instrumentation
    // and MUST NOT be wrapped in closure (`let __otel_res: Result<_, _> = (|| {`)
    assert!(!transformed.contains("__otel_res"));
    assert!(transformed.contains("let __otel_guard = __otel_cx.attach();"));
    assert!(!transformed.contains("__otel_cx.clone().attach()"));
}

#[test]
fn test_native_otel_type_aliased_mut_ref_fallback() {
    let source = r#"
pub type MutName<'a> = &'a mut String;
pub struct Aliased {
    pub v: String,
}
impl Aliased {
    pub fn alias_mut(&mut self) -> Result<MutName<'_>, String> {
        Ok(&mut self.v)
    }
}
"#;
    let report = analyze_source_str("aliased_crate", Path::new("src/lib.rs"), source)
        .expect("analyze source");
    assert_eq!(report.candidates.len(), 1);
    assert!(report.candidates[0].returns_result);
    assert!(report.candidates[0].returns_reference_or_lifetime);

    let transformed =
        transform_source_str_with_native_otel(source, "aliased_crate", &report.candidates)
            .expect("transform source");

    assert!(transformed.contains("/* __cargo_instrument_anchor: \"Aliased::alias_mut\" */"));
    assert!(transformed.contains("let __otel_guard = __otel_cx.attach();"));
    assert!(!transformed.contains("__otel_res"));
    assert!(!transformed.contains("redundant_closure_call"));
}

#[test]
fn test_native_otel_crlf_preservation() {
    let crlf_source = "pub fn crlf_sync() -> i32 {\r\n    let a = 100;\r\n    a\r\n}\r\n";
    assert_eq!(detect_line_ending(crlf_source), "\r\n");

    let report = analyze_source_str("crlf_crate", Path::new("src/lib.rs"), crlf_source)
        .expect("analyze CRLF");
    assert_eq!(report.candidates.len(), 1);

    let transformed =
        transform_source_str_with_native_otel(crlf_source, "crlf_crate", &report.candidates)
            .expect("transform CRLF");

    // Invariant: every newline in the transformed output must be preceded by \r
    let bare_lf_count = transformed
        .as_bytes()
        .windows(2)
        .filter(|w| w[1] == b'\n' && w[0] != b'\r')
        .count();
    assert_eq!(
        bare_lf_count, 0,
        "CRLF invariant violated: found bare LF without CR in transformed output"
    );

    assert!(transformed.contains("\r\n    /* __cargo_instrument_anchor: \"crlf_sync\" */\r\n"));
    assert!(transformed.contains("\r\n    let __otel_guard = __otel_cx.attach();\r\n"));
}

#[test]
fn test_native_otel_in_memory_exporter_sdk_proof() {
    use opentelemetry::trace::TracerProvider as _;
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let tracer = provider.tracer("direct_test_scope");

    // Exercise the exact pattern generated by NativeOtelEmitter
    let mut span = opentelemetry::trace::Tracer::span_builder(&tracer, "direct_proof")
        .with_kind(opentelemetry::trace::SpanKind::Internal)
        .start(&tracer);
    span.set_status(opentelemetry::trace::Status::error(""));
    drop(span);

    let spans = exporter.get_finished_spans().expect("get finished spans");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "direct_proof");
    assert_eq!(spans[0].instrumentation_scope.name(), "direct_test_scope");
    assert_eq!(spans[0].span_kind, opentelemetry::trace::SpanKind::Internal);
    assert_eq!(spans[0].status, opentelemetry::trace::Status::error(""));
}

#[test]
fn test_native_otel_live_rustc_and_clippy_compilation_proof() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let project_root = temp_dir.path().join("proof_crate");
    let src_dir = project_root.join("src");
    let tests_dir = project_root.join("tests");
    fs::create_dir_all(&src_dir).expect("create src dir");
    fs::create_dir_all(&tests_dir).expect("create tests dir");

    let cargo_toml = r#"
[package]
name = "proof_crate"
version = "0.1.0"
edition = "2021"

[dependencies]
opentelemetry = "0.32.0"

[dev-dependencies]
opentelemetry_sdk = { version = "0.32.0", features = ["testing"] }
"#;
    fs::write(project_root.join("Cargo.toml"), cargo_toml).expect("write Cargo.toml");

    // Comprehensive function shapes in a single source file including C1 and unsafe Result
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

// 10. C1 Inherent Method: &mut self returning Result<&mut String, String>
pub struct Holder {
    pub name: String,
}

impl Holder {
    pub fn get_mut(&mut self) -> Result<&mut String, String> {
        Ok(&mut self.name)
    }
}

// 11. C1 Free Function: &mut [u8] returning Result<&mut u8, String>
pub fn nth_mut(s: &mut [u8], i: usize) -> Result<&mut u8, String> {
    s.get_mut(i).ok_or_else(|| "x".into())
}

// 12. Unsafe function returning Result
/// # Safety
/// Caller must ensure pointer is valid if non-null.
pub unsafe fn unsafe_fallible(ptr: *const i32) -> Result<i32, String> {
    if ptr.is_null() {
        Err("null".into())
    } else {
        Ok(*ptr)
    }
}

// 13. Type-aliased mutable reference return (aliased &mut)
pub type MutName<'a> = &'a mut String;
pub struct Aliased {
    pub v: String,
}
impl Aliased {
    pub fn alias_mut(&mut self) -> Result<MutName<'_>, String> {
        Ok(&mut self.v)
    }
}
"#;

    // 1. Analyze candidates
    let report =
        analyze_source_str("proof_crate", Path::new("src/lib.rs"), source).expect("analyze source");
    assert_eq!(report.candidates.len(), 14);

    // Verify C1 & aliased mutable reference candidates
    let holder_get_mut = report
        .candidates
        .iter()
        .find(|c| c.function_name == "Holder::get_mut")
        .unwrap();
    assert!(holder_get_mut.returns_mut_reference);
    assert!(holder_get_mut.returns_reference_or_lifetime);

    let nth_mut_c = report
        .candidates
        .iter()
        .find(|c| c.function_name == "nth_mut")
        .unwrap();
    assert!(nth_mut_c.returns_mut_reference);
    assert!(nth_mut_c.returns_reference_or_lifetime);

    let alias_mut_c = report
        .candidates
        .iter()
        .find(|c| c.function_name == "Aliased::alias_mut")
        .unwrap();
    assert!(alias_mut_c.returns_reference_or_lifetime);

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

    // 5. Live verification 3 (H1 runtime execution proof):
    // Write an integration test using InMemorySpanExporter to prove runtime execution,
    // span creation, kind == Internal, scope name attribution, and Err -> Error / Ok -> Unset status
    let runtime_test_code = r#"
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use proof_crate::*;

#[test]
fn test_runtime_spans_execution() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let _ = opentelemetry::global::set_tracer_provider(provider);

    // 1. Plain sync non-Result function: 2 calls
    assert_eq!(non_result(10), 11);
    assert_eq!(non_result(20), 21);

    // 2. Fallible calls returning Err: 2 calls
    assert!(early_return(-5).is_err());
    assert!(early_return(-10).is_err());

    // 3. Fallible calls returning Ok: 2 calls
    assert_eq!(early_return(5), Ok(10));
    assert_eq!(early_return(10), Ok(20));

    // 4. C1 &mut accessor: 1 call
    let mut h = Holder { name: "initial".to_string() };
    let r = h.get_mut().expect("get_mut should succeed");
    r.push_str("_updated");
    assert_eq!(h.name, "initial_updated");

    // 5. Aliased &mut accessor: 1 call
    let mut a = Aliased { v: "initial".to_string() };
    let m = a.alias_mut().expect("alias_mut should succeed");
    m.push_str("_aliased");
    assert_eq!(a.v, "initial_aliased");

    // Retrieve spans from in-memory exporter
    let spans = exporter.get_finished_spans().expect("get finished spans");
    assert_eq!(spans.len(), 8, "expected exactly 8 spans for 8 calls");

    // Spans 0..2: non_result (Unset status, scope proof_crate, kind Internal)
    for s in &spans[0..2] {
        assert_eq!(s.name, "non_result");
        assert_eq!(s.instrumentation_scope.name(), "proof_crate");
        assert_eq!(s.span_kind, opentelemetry::trace::SpanKind::Internal);
        assert_eq!(s.status, opentelemetry::trace::Status::Unset);
    }

    // Spans 2..4: early_return returning Err (Error status per §16.10, scope proof_crate, kind Internal)
    for s in &spans[2..4] {
        assert_eq!(s.name, "early_return");
        assert_eq!(s.instrumentation_scope.name(), "proof_crate");
        assert_eq!(s.span_kind, opentelemetry::trace::SpanKind::Internal);
        assert_eq!(s.status, opentelemetry::trace::Status::error(""));
    }

    // Spans 4..6: early_return returning Ok (Unset status, scope proof_crate, kind Internal)
    for s in &spans[4..6] {
        assert_eq!(s.name, "early_return");
        assert_eq!(s.instrumentation_scope.name(), "proof_crate");
        assert_eq!(s.span_kind, opentelemetry::trace::SpanKind::Internal);
        assert_eq!(s.status, opentelemetry::trace::Status::Unset);
    }

    // Span 6: Holder::get_mut (C1 prefix-only fallback, Unset status, scope proof_crate, kind Internal)
    let s_mut = &spans[6];
    assert_eq!(s_mut.name, "Holder::get_mut");
    assert_eq!(s_mut.instrumentation_scope.name(), "proof_crate");
    assert_eq!(s_mut.span_kind, opentelemetry::trace::SpanKind::Internal);
    assert_eq!(s_mut.status, opentelemetry::trace::Status::Unset);

    // Span 7: Aliased::alias_mut (prefix-only fallback for aliased &mut, Unset status, scope proof_crate, kind Internal)
    let s_alias = &spans[7];
    assert_eq!(s_alias.name, "Aliased::alias_mut");
    assert_eq!(s_alias.instrumentation_scope.name(), "proof_crate");
    assert_eq!(s_alias.span_kind, opentelemetry::trace::SpanKind::Internal);
    assert_eq!(s_alias.status, opentelemetry::trace::Status::Unset);
}
"#;
    fs::write(tests_dir.join("runtime_test.rs"), runtime_test_code).expect("write runtime_test.rs");

    let test_output = Command::new("cargo")
        .arg("test")
        .current_dir(&project_root)
        .output()
        .expect("execute cargo test in proof_crate");
    assert!(
        test_output.status.success(),
        "Live cargo test failed on runtime proof! stderr:\n{}",
        String::from_utf8_lossy(&test_output.stderr)
    );
}

#[test]
fn test_native_otel_async_non_result_shape() {
    let source = r#"
pub async fn fetch_data(id: u64) -> String {
    let url = format!("https://example.com/{}", id);
    url
}
"#;
    let report =
        analyze_source_str("async_crate", Path::new("src/lib.rs"), source).expect("analyze source");
    assert_eq!(report.candidates.len(), 1);
    assert!(report.candidates[0].is_async);
    assert!(!report.candidates[0].returns_result);

    let transformed =
        transform_source_str_with_native_otel(source, "async_crate", &report.candidates)
            .expect("transform source");

    assert!(transformed.contains("/* __cargo_instrument_anchor: \"fetch_data\" */"));
    assert!(
        transformed.contains("let __otel_tracer = opentelemetry::global::tracer(\"async_crate\");")
    );
    assert!(transformed.contains("let __otel_span = opentelemetry::trace::Tracer::span_builder(&__otel_tracer, \"fetch_data\")"));
    assert!(transformed.contains(".with_kind(opentelemetry::trace::SpanKind::Internal)"));
    assert!(transformed.contains(".start(&__otel_tracer);"));
    assert!(transformed.contains("let __otel_cx = <opentelemetry::Context as opentelemetry::trace::TraceContextExt>::current_with_span(__otel_span);"));
    assert!(transformed.contains("opentelemetry::trace::FutureExt::with_context(async move {"));
    assert!(transformed.contains("}, __otel_cx).await"));

    // Invariant: no closure wrapping, no __otel_res, no redundant clippy allows
    assert!(!transformed.contains("__otel_res"));
    assert!(!transformed.contains("clippy::redundant_async_block"));
    assert!(!transformed.contains("clippy::redundant_closure_call"));

    // Body preserved
    assert!(transformed.contains("let url = format!(\"https://example.com/{}\", id);"));
}

#[test]
fn test_native_otel_async_result_shape() {
    let source = r#"
pub async fn query_user(id: u64) -> Result<String, &'static str> {
    if id == 0 {
        return Err("not found");
    }
    Ok("user".to_string())
}
"#;
    let report =
        analyze_source_str("async_crate", Path::new("src/lib.rs"), source).expect("analyze source");
    assert_eq!(report.candidates.len(), 1);
    assert!(report.candidates[0].is_async);
    assert!(report.candidates[0].returns_result);

    let transformed =
        transform_source_str_with_native_otel(source, "async_crate", &report.candidates)
            .expect("transform source");

    assert!(transformed.contains("/* __cargo_instrument_anchor: \"query_user\" */"));
    assert!(transformed.contains(
        "let __otel_res: Result<_, _> = opentelemetry::trace::FutureExt::with_context(async move {"
    ));
    assert!(transformed.contains("}, __otel_cx.clone()).await;"));
    assert!(transformed.contains("if __otel_res.is_err() {"));
    assert!(transformed.contains("opentelemetry::trace::TraceContextExt::span(&__otel_cx)"));
    assert!(transformed.contains(".set_status(opentelemetry::trace::Status::error(\"\"));"));
    assert!(transformed.contains("__otel_res"));

    // Invariant: no closure wrap, no redundant clippy allows
    assert!(!transformed.contains("clippy::redundant_async_block"));
    assert!(!transformed.contains("redundant_closure_call"));
    assert!(!transformed.contains("let __otel_res: Result<_, _> = (|| {"));
}

#[test]
fn test_native_otel_async_mut_ref_preserves_error_recording() {
    let source = r#"
pub struct AsyncHolder {
    pub name: String,
}
impl AsyncHolder {
    pub async fn get_mut_async(&mut self) -> Result<&mut String, String> {
        Ok(&mut self.name)
    }
}
"#;
    let report =
        analyze_source_str("async_mut", Path::new("src/lib.rs"), source).expect("analyze source");
    assert_eq!(report.candidates.len(), 1);
    let c = &report.candidates[0];
    assert!(c.is_async);
    assert!(c.returns_result);
    assert!(c.returns_mut_reference);
    assert!(c.returns_reference_or_lifetime);

    let transformed =
        transform_source_str_with_native_otel(source, "async_mut", &report.candidates)
            .expect("transform source");

    // Invariant F3: unlike sync functions, async functions returning references DO NOT fall back
    // to prefix-only instrumentation; error status recording is fully preserved!
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"AsyncHolder::get_mut_async\" */"));
    assert!(transformed.contains(
        "let __otel_res: Result<_, _> = opentelemetry::trace::FutureExt::with_context(async move {"
    ));
    assert!(transformed.contains("}, __otel_cx.clone()).await;"));
    assert!(transformed.contains("if __otel_res.is_err() {"));
    assert!(transformed.contains("opentelemetry::trace::TraceContextExt::span(&__otel_cx)"));
    assert!(transformed.contains("__otel_res"));
}

#[test]
fn test_native_otel_async_methods_and_traits() {
    let source = r#"
pub struct AsyncWorker;

impl AsyncWorker {
    pub async fn process(&self, count: usize) -> usize {
        count * 10
    }
}

pub trait AsyncAction {
    async fn execute(&self);
}

impl AsyncAction for AsyncWorker {
    async fn execute(&self) {
        let _ = 1;
    }
}

pub async fn generic_run<T: std::fmt::Display>(val: T) -> String {
    format!("{}", val)
}
"#;
    let report = analyze_source_str("async_traits", Path::new("src/lib.rs"), source)
        .expect("analyze source");
    assert_eq!(report.candidates.len(), 3);

    let transformed =
        transform_source_str_with_native_otel(source, "async_traits", &report.candidates)
            .expect("transform source");

    assert!(transformed.contains("/* __cargo_instrument_anchor: \"AsyncWorker::process\" */"));
    assert!(transformed
        .contains("/* __cargo_instrument_anchor: \"<AsyncWorker as AsyncAction>::execute\" */"));
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"generic_run\" */"));

    let matches = transformed
        .matches("opentelemetry::trace::FutureExt::with_context(async move {")
        .count();
    assert_eq!(matches, 3);
}

#[test]
fn test_native_otel_async_idempotence() {
    let source = r#"
pub async fn handle_task(task_id: u64) -> Result<String, &'static str> {
    if task_id == 0 {
        return Err("zero");
    }
    Ok("done".to_string())
}
"#;
    let report1 = analyze_source_str("idem_crate", Path::new("src/lib.rs"), source)
        .expect("analyze source first pass");
    assert_eq!(report1.candidates.len(), 1);

    let transformed1 =
        transform_source_str_with_native_otel(source, "idem_crate", &report1.candidates)
            .expect("transform first pass");

    let report2 = analyze_source_str("idem_crate", Path::new("src/lib.rs"), &transformed1)
        .expect("analyze source second pass");

    assert_eq!(report2.candidates.len(), 0);

    let plan = TransformationPlan::build_with_emitter(
        &transformed1,
        &report1.candidates,
        &NativeOtelEmitter::new("idem_crate"),
    )
    .expect("build plan on already instrumented text");
    assert_eq!(plan.edits.len(), 0);
    assert_eq!(plan.skipped.len(), 1);
    assert_eq!(plan.skipped[0].reason, SkipReason::AlreadyInstrumented);
}

#[test]
fn test_native_otel_async_crlf_and_unicode_preservation() {
    let source = "pub async fn crlf_async() -> i32 {\r\n    // 🦀 Concurrent async task\r\n    let val = 42;\r\n    val\r\n}\r\n";
    assert_eq!(detect_line_ending(source), "\r\n");

    let report = analyze_source_str("crlf_async_crate", Path::new("src/lib.rs"), source)
        .expect("analyze CRLF async");
    assert_eq!(report.candidates.len(), 1);

    let transformed =
        transform_source_str_with_native_otel(source, "crlf_async_crate", &report.candidates)
            .expect("transform CRLF async");

    let bare_lf_count = transformed
        .as_bytes()
        .windows(2)
        .filter(|w| w[1] == b'\n' && w[0] != b'\r')
        .count();
    assert_eq!(
        bare_lf_count, 0,
        "CRLF invariant violated in async output: found bare LF without CR"
    );
    assert!(transformed.contains("\r\n    /* __cargo_instrument_anchor: \"crlf_async\" */\r\n"));
    assert!(transformed.contains("// 🦀 Concurrent async task"));
    assert!(transformed.contains("}, __otel_cx).await\r\n"));
}

#[test]
fn test_native_otel_async_live_runtime_proof() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let project_root = temp_dir.path().join("async_proof_crate");
    let src_dir = project_root.join("src");
    let tests_dir = project_root.join("tests");
    fs::create_dir_all(&src_dir).expect("create src dir");
    fs::create_dir_all(&tests_dir).expect("create tests dir");

    let cargo_toml = r#"
[package]
name = "async_proof_crate"
version = "0.1.0"
edition = "2021"

[dependencies]
async-trait = "0.1"
opentelemetry = "0.32.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "time"] }

[dev-dependencies]
opentelemetry_sdk = { version = "0.32.0", features = ["testing"] }
"#;
    fs::write(project_root.join("Cargo.toml"), cargo_toml).expect("write Cargo.toml");

    let source = r#"#![deny(warnings)]

use std::time::Duration;

// 1. Single await non-result (F1 clippy check)
pub async fn single_await_nonresult(n: u64) -> u64 {
    tokio::time::sleep(Duration::from_millis(1)).await;
    n + 1
}

// 2. Multiple await points (Matrix item 2 / S1)
pub async fn multi_await(n: u64) -> u64 {
    tokio::time::sleep(Duration::from_millis(1)).await;
    tokio::time::sleep(Duration::from_millis(1)).await;
    tokio::time::sleep(Duration::from_millis(1)).await;
    tokio::time::sleep(Duration::from_millis(1)).await;
    tokio::time::sleep(Duration::from_millis(1)).await;
    n + 5
}

// 3. Fallible op with early return and ? (Matrix items 4 & 5)
pub async fn fallible_op(x: i32) -> Result<i32, &'static str> {
    if x < 0 {
        return Err("negative input");
    }
    let val = single_await_nonresult(x as u64).await;
    Ok(val as i32)
}

// 4. Nested async calls (Matrix item 6 / S3 parent-child hierarchy)
pub async fn nested_inner(val: u32) -> u32 {
    tokio::time::sleep(Duration::from_millis(2)).await;
    val * 2
}

pub async fn nested_outer(val: u32) -> u32 {
    nested_inner(val).await + 10
}

// 5. 100ms sleeper for duration measurement (§16.7 wall-clock proof)
pub async fn sleeper_100ms() {
    tokio::time::sleep(Duration::from_millis(100)).await;
}

// 6. Long-running task for cancellation (§16.12 drop proof)
pub async fn long_running() {
    tokio::time::sleep(Duration::from_secs(30)).await;
}

// 7. Method on struct returning Result<&mut String, ()> (F3 &mut async accessor)
pub struct Store {
    pub data: String,
}
impl Store {
    pub async fn get_mut_async(&mut self) -> Result<&mut String, ()> {
        tokio::time::sleep(Duration::from_millis(1)).await;
        Ok(&mut self.data)
    }
}

// 8. Generic async function
pub async fn generic_async<T: std::fmt::Display>(val: T) -> String {
    tokio::time::sleep(Duration::from_millis(1)).await;
    format!("generic: {}", val)
}

// 9. #[async_trait] implementation (R1 regression verification)
#[async_trait::async_trait]
pub trait AsyncProcessor {
    async fn process_job(&self, id: u32) -> Result<u32, ()>;
}

pub struct ProcessorImpl;

#[async_trait::async_trait]
impl AsyncProcessor for ProcessorImpl {
    async fn process_job(&self, id: u32) -> Result<u32, ()> {
        tokio::time::sleep(Duration::from_millis(1)).await;
        Ok(id * 3)
    }
}
"#;

    let report = analyze_source_str("async_proof_crate", Path::new("src/lib.rs"), source)
        .expect("analyze source");
    assert_eq!(report.candidates.len(), 10);

    let transformed =
        transform_source_str_with_native_otel(source, "async_proof_crate", &report.candidates)
            .expect("transform source");
    fs::write(src_dir.join("lib.rs"), transformed).expect("write transformed lib.rs");

    // 1. First run clippy with -D warnings on the generated code (proves F1 / Matrix item 14)
    let clippy_output = Command::new("cargo")
        .args(["clippy", "--all-targets", "--", "-D", "warnings"])
        .current_dir(&project_root)
        .output()
        .expect("execute cargo clippy in async_proof_crate");
    assert!(
        clippy_output.status.success(),
        "Clippy -D warnings failed on generated async code! stderr:\n{}",
        String::from_utf8_lossy(&clippy_output.stderr)
    );

    // 2. Write the async multi-thread runtime test
    let runtime_test_code = r#"
use async_proof_crate::*;
use opentelemetry::trace::TraceContextExt as _;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_async_matrix() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let _ = opentelemetry::global::set_tracer_provider(provider);

    // Matrix 1 & 10: Basic async fn & Send bound in tokio::spawn
    let handle = tokio::spawn(async {
        single_await_nonresult(42).await
    });
    let res = handle.await.expect("spawned task should finish");
    assert_eq!(res, 43);

    // Matrix 2: Multiple await points (S1: 5 await points)
    let res = multi_await(10).await;
    assert_eq!(res, 15);

    // Matrix 3: Multiple sequential calls (S1: 3 invocations -> 3 spans)
    assert_eq!(single_await_nonresult(1).await, 2);
    assert_eq!(single_await_nonresult(2).await, 3);
    assert_eq!(single_await_nonresult(3).await, 4);

    // Matrix 4 & 5: Fallible Err vs Ok & early return + ?
    assert!(fallible_op(-10).await.is_err());
    assert_eq!(fallible_op(5).await, Ok(6));

    // Matrix 6: Nested async calls (S3 parent-child hierarchy)
    assert_eq!(nested_outer(5).await, 20);

    // Matrix 8: Context cleanup after completion (S5)
    assert!(!opentelemetry::Context::current().has_active_span());

    // Matrix 9: Cancellation mid-flight (§16.12 drop proof)
    let cancel_handle = tokio::spawn(long_running());
    tokio::time::sleep(Duration::from_millis(20)).await;
    cancel_handle.abort();
    let _ = cancel_handle.await;

    // Matrix 11: Span duration measurement (§16.7 wall-clock proof)
    let t0 = std::time::Instant::now();
    sleeper_100ms().await;
    let elapsed = t0.elapsed();
    assert!(elapsed >= Duration::from_millis(90));

    // F3: Method on struct with &mut async accessor
    let mut store = Store { data: "hello".to_string() };
    let r = store.get_mut_async().await.expect("get_mut_async should succeed");
    r.push_str("_world");
    assert_eq!(store.data, "hello_world");

    // Matrix 12: Generic async function
    assert_eq!(generic_async(99).await, "generic: 99");

    // Matrix 16: #[async_trait] implementation (R1 regression check)
    let proc = ProcessorImpl;
    assert_eq!(proc.process_job(7).await, Ok(21));

    // Matrix 7: Concurrent tasks (S6 no cross-adoption)
    let mut handles = Vec::new();
    for i in 0..10 {
        handles.push(tokio::spawn(async move {
            single_await_nonresult(i).await
        }));
    }
    for h in handles {
        h.await.expect("concurrent task should finish");
    }

    // Inspect finished spans from the in-memory exporter
    let spans = exporter.get_finished_spans().expect("get finished spans");

    // Matrix 1: single_await_nonresult properties
    let s_basic = spans.iter().find(|s| s.name == "single_await_nonresult").expect("find single_await span");
    assert_eq!(s_basic.instrumentation_scope.name(), "async_proof_crate");
    assert_eq!(s_basic.span_kind, opentelemetry::trace::SpanKind::Internal);
    assert_eq!(s_basic.status, opentelemetry::trace::Status::Unset);

    // Matrix 2: multi_await (5 await points must produce exactly 1 span per S1)
    let multi_spans: Vec<_> = spans.iter().filter(|s| s.name == "multi_await").collect();
    assert_eq!(multi_spans.len(), 1, "5 awaits must produce exactly 1 span (S1)");

    // Matrix 4: fallible_op error status
    let err_span = spans.iter().find(|s| s.name == "fallible_op" && s.status == opentelemetry::trace::Status::error("")).expect("find error span");
    assert_eq!(err_span.status, opentelemetry::trace::Status::error(""));

    // Matrix 6: nested calls S3 hierarchy
    let inner_span = spans.iter().find(|s| s.name == "nested_inner").expect("find inner span");
    let outer_span = spans.iter().find(|s| s.name == "nested_outer").expect("find outer span");
    assert_eq!(inner_span.parent_span_id, outer_span.span_context.span_id(), "S3 hierarchy: inner parent must match outer span_id");

    // Matrix 9: cancellation span: long_running aborted, but 1 span exported on drop (§16.12)
    let cancelled_spans: Vec<_> = spans.iter().filter(|s| s.name == "long_running").collect();
    assert_eq!(cancelled_spans.len(), 1, "cancelled future must export 1 span on drop (§16.12)");

    // Matrix 11: duration on sleeper_100ms span (§16.7 wall-clock proof)
    let sleep_span = spans.iter().find(|s| s.name == "sleeper_100ms").expect("find sleeper span");
    let span_dur = sleep_span.end_time.duration_since(sleep_span.start_time).expect("duration");
    assert!(span_dur >= Duration::from_millis(90), "span duration ({:?}) must reflect wall-clock sleep (§16.7)", span_dur);

    // Matrix 16: #[async_trait] span name
    let trait_span = spans.iter().find(|s| s.name.contains("process_job")).expect("find async_trait span");
    assert_eq!(trait_span.name, "<ProcessorImpl as AsyncProcessor>::process_job");
}
"#;
    fs::write(tests_dir.join("async_runtime_test.rs"), runtime_test_code)
        .expect("write async_runtime_test.rs");

    let test_output = Command::new("cargo")
        .arg("test")
        .current_dir(&project_root)
        .output()
        .expect("execute cargo test in async_proof_crate");
    assert!(
        test_output.status.success(),
        "Live cargo test failed on async runtime proof! stderr:\n{}",
        String::from_utf8_lossy(&test_output.stderr)
    );
}
