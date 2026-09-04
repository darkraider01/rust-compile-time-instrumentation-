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

pub fn already_instrumented_otel_body() {
    let span = tracer.start("my_span");
}

pub async fn already_instrumented_with_context() {
    async {}.with_context(cx).await;
}

pub fn uninstrumented_clean() {}
"#;

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), code)
        .expect("analysis should succeed");

    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].function_name, "uninstrumented_clean");
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
