use cargo_instrument::ast::analyze_source_str;
use cargo_instrument::candidate::{FunctionKind, UnsafePolicy};
use std::path::Path;

#[test]
fn test_discover_free_and_async_functions() {
    let code = r#"
pub fn sync_fn() -> i32 {
    42
}

pub async fn async_fn() -> String {
    "hello".to_string()
}
"#;

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code)
        .expect("analysis should succeed");

    assert_eq!(report.candidates.len(), 2);

    let sync_c = &report.candidates[0];
    assert_eq!(sync_c.function_name, "sync_fn");
    assert!(!sync_c.is_async);
    assert_eq!(sync_c.kind, FunctionKind::Free);

    let async_c = &report.candidates[1];
    assert_eq!(async_c.function_name, "async_fn");
    assert!(async_c.is_async);
    assert_eq!(async_c.kind, FunctionKind::Free);
}

#[test]
fn test_discover_methods_inherent_and_trait() {
    let code = r#"
struct Service;

impl Service {
    pub fn new() -> Self {
        Service
    }

    pub async fn process(&self) -> bool {
        true
    }
}

trait Worker {
    fn run(&self);
}

impl Worker for Service {
    fn run(&self) {
        println!("working");
    }
}
"#;

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code)
        .expect("analysis should succeed");

    assert_eq!(report.candidates.len(), 3);

    // Inherent new
    assert_eq!(report.candidates[0].function_name, "Service::new");
    assert!(!report.candidates[0].is_async);
    assert_eq!(
        report.candidates[0].kind,
        FunctionKind::InherentMethod {
            type_name: "Service".to_string()
        }
    );

    // Inherent process
    assert_eq!(report.candidates[1].function_name, "Service::process");
    assert!(report.candidates[1].is_async);
    assert_eq!(
        report.candidates[1].kind,
        FunctionKind::InherentMethod {
            type_name: "Service".to_string()
        }
    );

    // Trait method run
    assert_eq!(
        report.candidates[2].function_name,
        "<Service as Worker>::run"
    );
    assert_eq!(
        report.candidates[2].kind,
        FunctionKind::TraitMethod {
            trait_name: "Worker".to_string(),
            type_name: "Service".to_string(),
        }
    );
}

#[test]
fn test_discover_generic_functions_and_impls() {
    let code = r#"
pub fn generic_fn<T: Clone>(item: T) -> T {
    item
}

struct Container<T>(T);

impl<T> Container<T> {
    pub fn get(&self) -> &T {
        &self.0
    }

    pub fn transform<U>(&self, f: impl FnOnce(&T) -> U) -> U {
        f(&self.0)
    }
}
"#;

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code)
        .expect("analysis should succeed");

    assert_eq!(report.candidates.len(), 3);

    // Free generic fn
    assert!(report.candidates[0].is_generic);
    assert!(!report.candidates[0].has_enclosing_generics);

    // Method with no own generics in generic impl
    assert!(!report.candidates[1].is_generic);
    assert!(report.candidates[1].has_enclosing_generics);

    // Method with own generics in generic impl
    assert!(report.candidates[2].is_generic);
    assert!(report.candidates[2].has_enclosing_generics);
}

#[test]
fn test_nested_functions_skipped_per_section_12_2() {
    let code = r#"
pub fn outer_fn() {
    fn inner_nested() {
        println!("nested");
    }
    inner_nested();
}
"#;

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code)
        .expect("analysis should succeed");

    // Only outer_fn should be discovered, inner_nested must be excluded
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].function_name, "outer_fn");
}

#[test]
fn test_exclusions_const_and_extern() {
    let code = r#"
pub const fn const_fn(x: i32) -> i32 {
    x * 2
}

pub extern "C" fn extern_c_fn() -> i32 {
    0
}

pub fn regular_fn() -> i32 {
    1
}
"#;

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code)
        .expect("analysis should succeed");

    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].function_name, "regular_fn");
}

#[test]
fn test_exclusion_direct_self_recursion() {
    let code = r#"
pub fn recursive_fn(n: u32) -> u32 {
    if n == 0 {
        1
    } else {
        recursive_fn(n - 1)
    }
}

pub fn non_recursive_fn(n: u32) -> u32 {
    n + 1
}
"#;

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code)
        .expect("analysis should succeed");

    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].function_name, "non_recursive_fn");
}

#[test]
fn test_idempotence_r10() {
    let code = r#"
#[instrument]
pub fn already_instrumented_attr() {}

#[tracing::instrument(skip_all)]
pub fn already_instrumented_tracing() {}

#[tracing_attributes::instrument]
pub fn already_instrumented_tracing_attributes() {}

#[::tracing::instrument]
pub fn already_instrumented_rooted_tracing() {}

#[otel_instrument::instrument]
pub fn already_instrumented_otel_custom() {}

#[instrument_span]
pub fn already_instrumented_instrument_span() {}

#[propagate_context]
pub fn already_instrumented_propagate_sync() {}

#[propagate_context]
pub async fn already_instrumented_propagate_async() {}

pub fn already_instrumented_otel_body() {
    let span = tracer.start("my_span");
}

pub async fn already_instrumented_with_context() {
    async {}.with_context(cx).await;
}

pub fn uninstrumented_clean() {}

#[my_crate::propagate_config]
pub fn uninstrumented_unrelated_attribute() {}

pub fn instrument() {}

pub fn propagate_context() {}
"#;

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code)
        .expect("analysis should succeed");

    // 4 clean candidates: uninstrumented_clean, uninstrumented_unrelated_attribute, instrument, propagate_context
    assert_eq!(report.candidates.len(), 4);
    let candidate_names: Vec<&str> = report
        .candidates
        .iter()
        .map(|c| c.function_name.as_str())
        .collect();
    assert!(candidate_names.contains(&"uninstrumented_clean"));
    assert!(candidate_names.contains(&"uninstrumented_unrelated_attribute"));
    assert!(candidate_names.contains(&"instrument"));
    assert!(candidate_names.contains(&"propagate_context"));

    // 10 functions skipped via handwritten_otel (8 attribute + 2 body)
    assert_eq!(report.skipped_stats.handwritten_otel, 10);
    // Total functions = 4 candidates + 10 skipped = 14
    assert_eq!(report.candidates.len() + report.skipped_stats.total(), 14);
}

#[test]
fn test_coexistence_two_class_attribute_matcher_widening() {
    let code = r#"
// Span-creating attributes
#[instrument]
fn f1() {}

#[tracing::instrument]
fn f2() {}

#[tracing_attributes::instrument]
fn f3() {}

#[::tracing::instrument]
fn f4() {}

#[otel_instrument::instrument]
fn f5() {}

#[instrument_span]
fn f6() {}

// Context-propagating attributes (upstream contrib#791)
#[propagate_context]
fn f7() {}

#[custom_scope::propagate_context]
async fn f8() {}

// Negative cases — must be included as candidates
#[inline]
fn f9_inline() {}

#[allow(dead_code)]
fn f10_normal() {}

#[my_crate::propagate_config]
fn f11_unrelated_attr() {}

fn instrument() {}

fn propagate_context() {}
"#;

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code)
        .expect("analysis should succeed");

    // 8 functions skipped via handwritten_otel
    assert_eq!(report.skipped_stats.handwritten_otel, 8);
    // f9_inline is skipped via inline_attribute
    assert_eq!(report.skipped_stats.inline_attribute, 1);

    // Candidates: f10_normal, f11_unrelated_attr, instrument, propagate_context = 4
    assert_eq!(report.candidates.len(), 4);
    let names: Vec<&str> = report
        .candidates
        .iter()
        .map(|c| c.function_name.as_str())
        .collect();
    assert!(names.contains(&"f10_normal"));
    assert!(names.contains(&"f11_unrelated_attr"));
    assert!(names.contains(&"instrument"));
    assert!(names.contains(&"propagate_context"));

    // Reconciliation identity: candidates + skipped == total
    assert_eq!(report.candidates.len() + report.skipped_stats.total(), 13);
}

#[test]
fn test_unsafe_policy_detection() {
    let code_forbid = "#![forbid(unsafe_code)]\npub fn a() {}";
    let report_forbid =
        analyze_source_str("test_crate", Path::new("src/lib.rs"), code_forbid).unwrap();
    assert_eq!(report_forbid.unsafe_policy, UnsafePolicy::Forbidden);

    let code_deny = "#![deny(unsafe_code)]\npub fn b() {}";
    let report_deny = analyze_source_str("test_crate", Path::new("src/lib.rs"), code_deny).unwrap();
    assert_eq!(report_deny.unsafe_policy, UnsafePolicy::Denied);

    let code_clean = "pub fn c() {}";
    let report_clean =
        analyze_source_str("test_crate", Path::new("src/lib.rs"), code_clean).unwrap();
    assert_eq!(report_clean.unsafe_policy, UnsafePolicy::Allowed);
}

#[test]
fn test_c2_anyhow_with_context_is_candidate() {
    // Closure-based with_context is anyhow::Context, NOT opentelemetry::trace::FutureExt
    let code = r#"
pub fn load_config() -> Result<String, anyhow::Error> {
    read_file().with_context(|| "failed to read configuration")
}
"#;

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code)
        .expect("analysis should succeed");

    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].function_name, "load_config");
}

#[test]
fn test_c2_otel_with_context_is_excluded() {
    // Non-closure with_context is opentelemetry::trace::FutureExt::with_context
    let code = r#"
pub async fn send_telemetry() {
    async {}.with_context(cx).await;
}
"#;

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code)
        .expect("analysis should succeed");

    assert_eq!(report.candidates.len(), 0);
}

#[test]
fn test_c2_timer_start_and_ray_tracer_are_candidates() {
    // .start() without opentelemetry import and ray_tracer::render without bare "tracer" false positive
    let code = r#"
pub fn run_benchmark() {
    let mut timer = Timer::new();
    timer.start();
}

pub fn render_scene() {
    ray_tracer::render();
}
"#;

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code)
        .expect("analysis should succeed");

    assert_eq!(report.candidates.len(), 2);
    assert_eq!(report.candidates[0].function_name, "run_benchmark");
    assert_eq!(report.candidates[1].function_name, "render_scene");
}

#[test]
fn test_c2_tracer_start_with_otel_import_is_excluded() {
    let code = r#"
use opentelemetry::trace::Tracer;

pub fn perform_work(tracer: &impl Tracer) {
    let _span = tracer.start("perform_work");
}
"#;

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code)
        .expect("analysis should succeed");

    assert_eq!(report.candidates.len(), 0);
}

#[test]
fn test_h3_qualified_direct_self_recursion() {
    let code = r#"
pub struct Service;

impl Service {
    pub fn self_qualified_recurse(&self, n: u32) {
        if n > 0 {
            Self::self_qualified_recurse(self, n - 1);
        }
    }
}

pub fn crate_qualified_recurse(n: u32) {
    if n > 0 {
        crate::crate_qualified_recurse(n - 1);
    }
}

pub fn super_qualified_recurse(n: u32) {
    if n > 0 {
        super::super_qualified_recurse(n - 1);
    }
}

pub fn clean_fn() {}
"#;

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code)
        .expect("analysis should succeed");

    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].function_name, "clean_fn");
}

#[test]
fn test_h3_conservative_lookalike_recursion_tradeoff() {
    // Documented R7 tradeoff: calling OtherType::helper() inside fn helper() is treated as recursive
    let code = r#"
pub fn helper() {
    OtherType::helper();
}
"#;

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code)
        .expect("analysis should succeed");

    // Per R7, intentionally excluded as a conservative false positive
    assert_eq!(
        report.candidates.len(),
        0,
        "R7 conservative tradeoff: look-alike helper() is intentionally excluded"
    );
}

#[test]
fn test_c1_module_resolution_root_and_submodules() {
    use cargo_instrument::ast::analyze_source_file;
    use std::fs;

    let temp_dir = tempfile::tempdir().expect("tempdir");
    let src = temp_dir.path().join("src");
    fs::create_dir_all(&src).expect("create src");

    let lib_code = "pub mod helpers;\npub fn lib_fn() {}\n";
    let helpers_code = "pub fn helper_fn() {}\n";

    fs::write(src.join("lib.rs"), lib_code).expect("write lib.rs");
    fs::write(src.join("helpers.rs"), helpers_code).expect("write helpers.rs");

    let report = analyze_source_file("my_crate", &src.join("lib.rs")).expect("analyze");
    assert_eq!(report.candidates.len(), 2);
    let names: Vec<&str> = report
        .candidates
        .iter()
        .map(|c| c.function_name.as_str())
        .collect();
    assert!(names.contains(&"lib_fn"));
    assert!(names.contains(&"helper_fn"));
}

#[test]
fn test_c1_module_resolution_non_main_root() {
    use cargo_instrument::ast::analyze_source_file;
    use std::fs;

    // C1: Test that a non-main crate root (e.g. src/bin/other_tool.rs) resolves submodules as siblings!
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let bin_dir = temp_dir.path().join("src").join("bin");
    fs::create_dir_all(&bin_dir).expect("create bin");

    let tool_code = "mod tool_sibling;\npub fn tool_main() {}\n";
    let sibling_code = "pub fn sibling_fn() {}\n";

    fs::write(bin_dir.join("other_tool.rs"), tool_code).expect("write other_tool.rs");
    fs::write(bin_dir.join("tool_sibling.rs"), sibling_code).expect("write tool_sibling.rs");

    let report =
        analyze_source_file("other_tool", &bin_dir.join("other_tool.rs")).expect("analyze");
    assert_eq!(report.candidates.len(), 2);
    let names: Vec<&str> = report
        .candidates
        .iter()
        .map(|c| c.function_name.as_str())
        .collect();
    assert!(names.contains(&"tool_main"));
    assert!(names.contains(&"sibling_fn"));
}

#[test]
fn test_c1_module_resolution_three_level_nested() {
    use cargo_instrument::ast::analyze_source_file;
    use std::fs;

    let temp_dir = tempfile::tempdir().expect("tempdir");
    let src = temp_dir.path().join("src");
    let a_dir = src.join("a");
    fs::create_dir_all(&a_dir).expect("create a_dir");

    fs::write(src.join("main.rs"), "mod a;\nfn main() {}\n").expect("write main.rs");
    fs::write(a_dir.join("mod.rs"), "pub mod b;\npub fn a_fn() {}\n").expect("write a/mod.rs");
    fs::write(a_dir.join("b.rs"), "pub fn b_fn() {}\n").expect("write a/b.rs");

    let report = analyze_source_file("app", &src.join("main.rs")).expect("analyze");
    assert_eq!(report.candidates.len(), 3);
    let names: Vec<&str> = report
        .candidates
        .iter()
        .map(|c| c.function_name.as_str())
        .collect();
    assert!(names.contains(&"main"));
    assert!(names.contains(&"a_fn"));
    assert!(names.contains(&"b_fn"));
}

#[test]
fn test_c1_module_resolution_path_attribute() {
    use cargo_instrument::ast::analyze_source_file;
    use std::fs;

    let temp_dir = tempfile::tempdir().expect("tempdir");
    let src = temp_dir.path().join("src");
    let custom_dir = src.join("custom");
    fs::create_dir_all(&custom_dir).expect("create custom");

    fs::write(
        src.join("lib.rs"),
        r#"#[path = "custom/my_file.rs"] mod custom; pub fn root_fn() {}"#,
    )
    .expect("write lib.rs");
    fs::write(custom_dir.join("my_file.rs"), "pub fn custom_fn() {}\n").expect("write my_file.rs");

    let report = analyze_source_file("app", &src.join("lib.rs")).expect("analyze");
    assert_eq!(report.candidates.len(), 2);
    let names: Vec<&str> = report
        .candidates
        .iter()
        .map(|c| c.function_name.as_str())
        .collect();
    assert!(names.contains(&"root_fn"));
    assert!(names.contains(&"custom_fn"));
}

#[test]
fn test_ast_error_io_on_missing_file() {
    use cargo_instrument::ast::{analyze_source_file, AstError};
    let missing = Path::new("non_existent_file_xyz_123.rs");
    let result = analyze_source_file("test_crate", missing);
    match result {
        Err(AstError::Io { path, .. }) => {
            assert_eq!(path, missing);
        }
        other => panic!("expected AstError::Io, got {:?}", other),
    }
}

#[test]
fn test_ast_error_parse_on_invalid_syntax() {
    use cargo_instrument::ast::{analyze_source_str, AstError};
    let bad_code = "fn broken( { let x = ; }";
    let result = analyze_source_str("test_crate", Path::new("test.rs"), bad_code);
    match result {
        Err(AstError::Parse { path, .. }) => {
            assert_eq!(path, Path::new("test.rs"));
        }
        other => panic!("expected AstError::Parse, got {:?}", other),
    }
}

#[test]
fn test_discovery_report_format_debug() {
    use cargo_instrument::candidate::{Candidate, DiscoveryReport, FunctionKind, UnsafePolicy};
    use std::path::PathBuf;

    // 1. Empty candidates
    let empty_report = DiscoveryReport {
        crate_name: "empty_crate".to_string(),
        source_file: PathBuf::from("src/lib.rs"),
        unsafe_policy: UnsafePolicy::Allowed,
        is_no_std: false,
        has_colliding_symbols: false,
        skipped_stats: Default::default(),
        candidates: Vec::new(),
    };
    let formatted_empty = empty_report.format_debug();
    assert!(formatted_empty.contains("crate: empty_crate"));
    assert!(formatted_empty.contains("unsafe_policy: Allowed"));
    assert!(formatted_empty.contains("(none)"));

    // 2. Non-empty candidates
    let report_with_candidate = DiscoveryReport {
        crate_name: "sample_crate".to_string(),
        source_file: PathBuf::from("src/main.rs"),
        unsafe_policy: UnsafePolicy::Forbidden,
        is_no_std: true,
        has_colliding_symbols: false,
        skipped_stats: Default::default(),
        candidates: vec![Candidate {
            function_name: "compute".to_string(),
            source_file: PathBuf::from("src/main.rs"),
            byte_range: 10..50,
            body_byte_range: 25..50,
            kind: FunctionKind::Free,
            is_async: true,
            is_generic: false,
            has_enclosing_generics: false,
            returns_result: false,
            returns_mut_reference: false,
            returns_reference_or_lifetime: false,
        }],
    };
    let formatted_c = report_with_candidate.format_debug();
    assert!(formatted_c.contains("crate: sample_crate"));
    assert!(formatted_c.contains("unsafe_policy: Forbidden"));
    assert!(formatted_c.contains("compute: bytes 10..50 (src/main.rs)"));
}

#[test]
fn test_trait_impl_method_function_name_normalization() {
    let code = r#"
pub struct MyErr(std::num::ParseIntError);

impl From<std::num::ParseIntError> for MyErr {
    fn from(e: std::num::ParseIntError) -> Self {
        MyErr(e)
    }
}
"#;

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code)
        .expect("analysis should succeed");

    assert_eq!(report.candidates.len(), 1);
    assert_eq!(
        report.candidates[0].function_name,
        "<MyErr as From<std::num::ParseIntError>>::from"
    );
    assert!(!report.candidates[0].function_name.contains(" :: "));
    assert!(!report.candidates[0].function_name.contains(" < "));
    assert!(!report.candidates[0].function_name.contains(" > "));
    assert!(!report.candidates[0].function_name.contains(" >>"));
}

#[test]
fn test_returns_mut_reference_detection() {
    let code = r#"
pub struct MyStruct;

impl MyStruct {
    pub fn get_mut(&mut self) -> Result<&mut String, String> {
        todo!()
    }
    pub fn get_ref(&self) -> Result<&String, String> {
        todo!()
    }
    pub fn raw_mut(&mut self) -> &mut i32 {
        todo!()
    }
    pub fn plain(&self) -> i32 {
        42
    }
}

pub fn slice_mut(s: &mut [u8]) -> Result<&mut u8, String> {
    todo!()
}
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code)
        .expect("analysis should succeed");

    assert_eq!(report.candidates.len(), 5);
    assert!(report.candidates[0].returns_mut_reference); // get_mut
    assert!(!report.candidates[1].returns_mut_reference); // get_ref
    assert!(report.candidates[2].returns_mut_reference); // raw_mut
    assert!(!report.candidates[3].returns_mut_reference); // plain
    assert!(report.candidates[4].returns_mut_reference); // slice_mut

    // returns_reference_or_lifetime covers both &mut, shared references, and lifetimes
    assert!(report.candidates[0].returns_reference_or_lifetime); // get_mut
    assert!(report.candidates[1].returns_reference_or_lifetime); // get_ref
    assert!(report.candidates[2].returns_reference_or_lifetime); // raw_mut
    assert!(!report.candidates[3].returns_reference_or_lifetime); // plain
    assert!(report.candidates[4].returns_reference_or_lifetime); // slice_mut
}

#[test]
fn test_returns_reference_or_lifetime_type_alias_mut() {
    let code = r#"
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
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code)
        .expect("analysis should succeed");
    assert_eq!(report.candidates.len(), 1);
    assert!(report.candidates[0].returns_result);
    assert!(report.candidates[0].returns_reference_or_lifetime);
}

#[test]
fn test_drop_implementation_excluded_and_counted() {
    let code = r#"
pub struct Resource;
impl Drop for Resource {
    fn drop(&mut self) {
        println!("cleanup");
    }
}
pub fn ordinary_fn() {}
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code).unwrap();
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].function_name, "ordinary_fn");
    assert_eq!(report.skipped_stats.drop_implementation, 1);
}

#[test]
fn test_generic_adapter_traits_excluded_and_counted() {
    let code = r#"
pub struct Wrapper<T>(pub T);
impl<T> std::ops::Deref for Wrapper<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
impl<T> AsRef<T> for Wrapper<T> {
    fn as_ref(&self) -> &T {
        &self.0
    }
}
impl Borrow<str> for Wrapper<String> {
    fn borrow(&self) -> &str {
        &self.0
    }
}
pub fn business_logic() {}
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code).unwrap();
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].function_name, "business_logic");
    assert_eq!(report.skipped_stats.adapter_trait, 3);
}

#[test]
fn test_inline_attribute_exclusion_and_inline_never_eligibility() {
    let code = r#"
#[inline]
pub fn inlined_fn() -> i32 { 1 }

#[inline(always)]
pub fn inlined_always_fn() -> i32 { 2 }

#[inline(never)]
pub fn out_of_line_fn() -> i32 { 3 }

pub fn plain_fn() -> i32 { 4 }
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code).unwrap();
    let names: Vec<_> = report
        .candidates
        .iter()
        .map(|c| c.function_name.as_str())
        .collect();
    assert_eq!(names, vec!["out_of_line_fn", "plain_fn"]);
    assert_eq!(report.skipped_stats.inline_attribute, 2);
}

#[test]
fn test_cfg_test_recursive_function_tally() {
    let code = r#"
pub fn prod_a() {}
pub fn prod_b() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_one() {}

    #[test]
    fn test_two() {}

    struct TestHelper;
    impl TestHelper {
        fn helper_method(&self) {}
    }

    mod nested_tests {
        fn deep_test_helper() {}
    }
}

#[test]
fn standalone_test() {}
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code).unwrap();
    assert_eq!(report.candidates.len(), 2);
    // 4 inside `mod tests` (test_one, test_two, helper_method, deep_test_helper) + 1 standalone_test = 5
    assert_eq!(report.skipped_stats.cfg_test, 5);
}

#[test]
fn test_nested_function_tally() {
    let code = r#"
pub fn outer_function() {
    fn inner_helper_one() {}
    fn inner_helper_two() {}
    inner_helper_one();
    inner_helper_two();
}
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code).unwrap();
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].function_name, "outer_function");
    assert_eq!(report.skipped_stats.nested_function, 2);
}

#[test]
fn test_universal_reconciliation_synthetic_fixture() {
    let code = r#"
pub fn ordinary_fn() -> i32 {
    fn nested_helper() -> i32 { 10 }
    nested_helper()
}

pub const fn const_calc() -> i32 { 42 }

pub extern "C" fn extern_abi_fn() {}

#[inline]
pub fn inlined_fast() {}

pub fn self_rec(n: u32) -> u32 {
    if n == 0 { 0 } else { self_rec(n - 1) }
}

pub struct Item;
impl Drop for Item {
    fn drop(&mut self) {}
}

impl std::ops::Deref for Item {
    type Target = ();
    fn deref(&self) -> &() { &() }
}

#[cfg(test)]
mod tests {
    fn test_one() {}
    fn test_two() {}
}
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code).unwrap();
    let syn_file = syn::parse_file(code).unwrap();
    let total_fns = cargo_instrument::ast::count_total_functions(&syn_file);

    assert_eq!(
        report.candidates.len() + report.skipped_stats.total(),
        total_fns,
        "Reconciliation invariant failed! candidates={} skipped={:?} total_fns={}",
        report.candidates.len(),
        report.skipped_stats,
        total_fns
    );
}

#[test]
fn test_census_exact_reconciliation() {
    let census_code = include_str!("fixtures/census_lib.rs");
    let report = analyze_source_str("census", Path::new("src/lib.rs"), census_code).unwrap();
    let syn_file = syn::parse_file(census_code).unwrap();
    let total_fns = cargo_instrument::ast::count_total_functions(&syn_file);

    // Exact reconciliation identity
    assert_eq!(
        report.candidates.len() + report.skipped_stats.total(),
        total_fns,
        "Census reconciliation failed! candidates={} skipped={:?} total_fns={}",
        report.candidates.len(),
        report.skipped_stats,
        total_fns
    );

    // Verify concrete construct class counts on real crate
    assert_eq!(
        report.skipped_stats.drop_implementation, 1,
        "Drop for InnerTrackedObject"
    );
    assert_eq!(
        report.skipped_stats.adapter_trait, 3,
        "Deref, AsRef<T>, Borrow<T>"
    );
    assert_eq!(
        report.skipped_stats.cfg_test, 9,
        "9 functions (8 #[test] + 1 helper) in #[cfg(test)] mod tests"
    );
    assert!(!report.is_no_std, "census is a pure std crate");
    assert!(!report.has_colliding_symbols, "census has no ABI collision");
    assert_eq!(
        report.candidates.len(),
        16,
        "Census has 16 eligible production functions"
    );
}

#[test]
fn test_abi_symbol_collision_detection() {
    let colliding_code = r#"
mod inner {
    #[no_mangle]
    pub extern "C" fn __otel_span_start() -> u64 { 0 }
}
pub fn normal() {}
"#;
    let report1 = analyze_source_str("colliding", Path::new("src/lib.rs"), colliding_code).unwrap();
    assert!(
        report1.has_colliding_symbols,
        "must detect #[no_mangle] __otel_span_start in inner module"
    );

    let mangled_code = r#"
fn __otel_span_enter() {}
pub fn normal() {}
"#;
    let report2 = analyze_source_str("mangled", Path::new("src/lib.rs"), mangled_code).unwrap();
    assert!(
        !report2.has_colliding_symbols,
        "mangled private helper must not be treated as ABI collision"
    );

    let export_name_code = r#"
#[export_name = "__otel_span_exit"]
pub extern "C" fn custom_exit(handle: u64) {}
"#;
    let report3 =
        analyze_source_str("export_name", Path::new("src/lib.rs"), export_name_code).unwrap();
    assert!(
        report3.has_colliding_symbols,
        "must detect #[export_name = ...] matching ABI symbols"
    );
}

#[test]
fn test_trampoline_emitter_symbols_subset_of_abi_symbols() {
    use cargo_instrument::ast::OTEL_ABI_SYMBOLS;
    use cargo_instrument::candidate::{Candidate, FunctionKind, UnsafePolicy};
    use cargo_instrument::transform::{Emitter, TrampolineEmitter};
    use std::path::PathBuf;

    let emitter = TrampolineEmitter::new("my_dep", Some("2021".to_string()), UnsafePolicy::Allowed);
    let candidate = Candidate {
        function_name: "test_fn".to_string(),
        source_file: PathBuf::from("src/lib.rs"),
        byte_range: 0..10,
        body_byte_range: 5..10,
        kind: FunctionKind::Free,
        is_async: false,
        is_generic: false,
        has_enclosing_generics: false,
        returns_result: true,
        returns_mut_reference: false,
        returns_reference_or_lifetime: false,
    };

    let prefix = emitter.emit_body_prefix(&candidate, "\n");
    // Extract every declared fn __otel_* in prefix
    for line in prefix.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("fn __otel_") {
            let fn_name = trimmed
                .strip_prefix("fn ")
                .unwrap()
                .split('(')
                .next()
                .unwrap()
                .trim();
            assert!(
                OTEL_ABI_SYMBOLS.contains(&fn_name),
                "TrampolineEmitter emitted symbol '{}' not in OTEL_ABI_SYMBOLS: {:?}",
                fn_name,
                OTEL_ABI_SYMBOLS
            );
        }
    }
}

#[test]
fn test_no_std_attribute_detection() {
    let unconditional = "#![no_std]\npub fn foo() {}\n";
    let report_uncond =
        analyze_source_str("uncond", Path::new("src/lib.rs"), unconditional).unwrap();
    assert!(
        report_uncond.is_no_std,
        "must detect unconditional #![no_std]"
    );

    let conditional_feature = "#![cfg_attr(not(feature = \"std\"), no_std)]\npub fn foo() {}\n";
    let report_feature =
        analyze_source_str("feat", Path::new("src/lib.rs"), conditional_feature).unwrap();
    assert!(
        report_feature.is_no_std,
        "must detect conditional #![cfg_attr(not(feature = \"std\"), no_std)]"
    );

    let conditional_std = "#![cfg_attr(not(std), no_std)]\npub fn foo() {}\n";
    let report_std =
        analyze_source_str("atoi_style", Path::new("src/lib.rs"), conditional_std).unwrap();
    assert!(
        report_std.is_no_std,
        "must detect #![cfg_attr(not(std), no_std)]"
    );

    let normal_crate = "pub fn foo() {}\n";
    let report_normal =
        analyze_source_str("std_crate", Path::new("src/lib.rs"), normal_crate).unwrap();
    assert!(
        !report_normal.is_no_std,
        "normal crate must not be flagged as no_std"
    );
}
