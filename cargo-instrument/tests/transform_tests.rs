use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cargo_instrument::ast::analyze_source_str;
use cargo_instrument::candidate::{Candidate, FunctionKind};
use cargo_instrument::transform::{
    detect_line_ending, paths_are_identical, transform_source_file, transform_source_file_scoped,
    transform_source_str, transform_source_str_with_emitter, Emitter, SkipReason, TransformError,
    TransformationPlan, INSTRUMENT_ANCHOR_PREFIX,
};

// ============================================================================
// Category A: Core Construct Transformation & Byte Preservation
// ============================================================================

#[test]
fn test_ordinary_function() {
    let source = "fn simple_add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analysis should succeed");
    assert_eq!(report.candidates.len(), 1);

    let transformed =
        transform_source_str(source, &report.candidates).expect("transformation should succeed");

    // Sentinel must be inserted
    assert!(transformed.contains(INSTRUMENT_ANCHOR_PREFIX));
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"simple_add\" */"));
    assert!(transformed.contains("let _cargo_instrument_sentinel = ();"));

    // Prefix before '{' must remain byte-for-byte identical
    let prefix = "fn simple_add(a: i32, b: i32) -> i32 {";
    assert!(transformed.starts_with(prefix));

    // Suffix including original body and closing '}' must remain intact
    assert!(transformed.ends_with("    a + b\n}\n"));
}

#[test]
fn test_async_function() {
    let source =
        "pub async fn fetch_data(url: &str) -> Result<String, ()> {\n    Ok(url.to_string())\n}\n";
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analysis should succeed");
    assert_eq!(report.candidates.len(), 1);

    let transformed =
        transform_source_str(source, &report.candidates).expect("transformation should succeed");

    assert!(transformed.contains("/* __cargo_instrument_anchor: \"fetch_data\" */"));
    assert!(transformed.contains("let _cargo_instrument_sentinel = ();"));
    assert!(transformed.starts_with("pub async fn fetch_data(url: &str) -> Result<String, ()> {"));
    assert!(transformed.ends_with("    Ok(url.to_string())\n}\n"));
}

#[test]
fn test_inherent_method() {
    let source = r#"
struct Calculator {
    base: i32,
}

impl Calculator {
    pub fn calculate(&self, factor: i32) -> i32 {
        self.base * factor
    }
}
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analysis should succeed");
    assert_eq!(report.candidates.len(), 1);

    let transformed =
        transform_source_str(source, &report.candidates).expect("transformation should succeed");

    assert!(transformed.contains("/* __cargo_instrument_anchor: \"Calculator::calculate\" */"));
    assert!(transformed.contains("let _cargo_instrument_sentinel = ();"));
    assert!(transformed.contains("self.base * factor"));
}

#[test]
fn test_trait_implementation_method() {
    let source = r#"
trait Greeter {
    fn greet(&self) -> &'static str;
}

struct Robot;

impl Greeter for Robot {
    fn greet(&self) -> &'static str {
        "beep boop"
    }
}
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analysis should succeed");
    assert_eq!(report.candidates.len(), 1);

    let transformed =
        transform_source_str(source, &report.candidates).expect("transformation should succeed");

    assert!(transformed.contains("/* __cargo_instrument_anchor: \"<Robot as Greeter>::greet\" */"));
    assert!(transformed.contains("let _cargo_instrument_sentinel = ();"));
    assert!(transformed.contains("\"beep boop\""));
}

#[test]
fn test_generic_function() {
    let source = r#"
pub fn clone_pair<'a, T: Clone, const N: usize>(items: &'a [T; N]) -> (T, T) {
    (items[0].clone(), items[1].clone())
}
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analysis should succeed");
    assert_eq!(report.candidates.len(), 1);

    let transformed =
        transform_source_str(source, &report.candidates).expect("transformation should succeed");

    assert!(transformed.contains("/* __cargo_instrument_anchor: \"clone_pair\" */"));
    assert!(transformed.contains("let _cargo_instrument_sentinel = ();"));
    assert!(transformed.contains("(items[0].clone(), items[1].clone())"));
}

#[test]
fn test_multiline_function() {
    let source = r#"
pub fn complex_process(val: Option<i32>) -> i32 {
    let mut total = 0;
    match val {
        Some(v) => {
            for i in 0..v {
                total += i;
            }
        }
        None => total = -1,
    }
    total
}
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analysis should succeed");
    assert_eq!(report.candidates.len(), 1);

    let transformed =
        transform_source_str(source, &report.candidates).expect("transformation should succeed");

    assert!(transformed.contains("/* __cargo_instrument_anchor: \"complex_process\" */"));
    assert!(transformed.contains("for i in 0..v {"));
    assert!(transformed.contains("total = -1,"));
}

#[test]
fn test_unusual_formatting() {
    let source = r#"
   fn    messy_spacing  < 'a > (
       x:   &'a   str
   )   ->   usize   {
       // Internal comment with messy indent
          x.len()
   }
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analysis should succeed");
    assert_eq!(report.candidates.len(), 1);

    let transformed =
        transform_source_str(source, &report.candidates).expect("transformation should succeed");

    // The messy spacing before '{' must remain 100% byte-identical
    assert!(transformed.contains(
        "   fn    messy_spacing  < 'a > (\n       x:   &'a   str\n   )   ->   usize   {"
    ));
    assert!(transformed.contains("// Internal comment with messy indent"));
    assert!(transformed.contains("          x.len()"));
}

#[test]
fn test_comments_preservation() {
    let source = r#"// Leading line comment
/* Leading block comment
   over multiple lines */

/// Doc comment on function
#[inline]
pub fn commented_fn() {
    // Inner body comment
    /* Inner block */
    let _a = 1;
}

// Trailing comment after function
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analysis should succeed");
    assert_eq!(report.candidates.len(), 1);

    let transformed =
        transform_source_str(source, &report.candidates).expect("transformation should succeed");

    // All outside comments must survive bit-for-bit
    assert!(transformed.starts_with("// Leading line comment\n/* Leading block comment\n   over multiple lines */\n\n/// Doc comment on function"));
    assert!(transformed.ends_with("// Trailing comment after function\n"));
    // Inner comments must survive untouched
    assert!(transformed.contains("// Inner body comment"));
    assert!(transformed.contains("/* Inner block */"));
}

#[test]
fn test_attributes_preservation() {
    let source = r#"
#[inline(always)]
#[allow(unused_variables)]
#[doc = "Custom doc attribute"]
pub fn attributed(x: i32) -> i32 {
    x
}
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analysis should succeed");
    assert_eq!(report.candidates.len(), 1);

    let transformed =
        transform_source_str(source, &report.candidates).expect("transformation should succeed");

    assert!(transformed.contains("#[inline(always)]\n#[allow(unused_variables)]\n#[doc = \"Custom doc attribute\"]\npub fn attributed(x: i32) -> i32 {"));
}

#[test]
fn test_unicode_before_function() {
    let prefix = "// 🦀 Ferrises and accents: é, ü, ç, 日本語\n// 🚀 Rocket line\n";
    let fn_text = "pub fn target_after_unicode() -> &'static str {\n    \"success\"\n}\n";
    let source = format!("{prefix}{fn_text}");

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), &source)
        .expect("analysis should succeed");
    assert_eq!(report.candidates.len(), 1);

    let transformed =
        transform_source_str(&source, &report.candidates).expect("transformation should succeed");

    // The exact UTF-8 prefix must be byte-for-byte identical
    assert!(transformed.starts_with(prefix));
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"target_after_unicode\" */"));
    assert!(transformed.contains("\"success\""));
}

#[test]
fn test_multiple_functions_in_one_file() {
    let source = r#"
fn first() -> i32 {
    1
}

// In-between comment
fn second() -> i32 {
    2
}

/* In-between block */
fn third() -> i32 {
    3
}
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analysis should succeed");
    assert_eq!(report.candidates.len(), 3);

    let transformed =
        transform_source_str(source, &report.candidates).expect("transformation should succeed");

    // All three functions must receive sentinels
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"first\" */"));
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"second\" */"));
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"third\" */"));

    // Interstitial comments and formatting must remain bit-for-bit identical
    assert!(transformed.contains("\n// In-between comment\nfn second() -> i32 {"));
    assert!(transformed.contains("\n/* In-between block */\nfn third() -> i32 {"));
}

// ============================================================================
// Category B: Exclusions & Sound Idempotence
// ============================================================================

#[test]
fn test_nested_function_exclusion() {
    let source = r#"
pub fn outer_fn() {
    fn nested_helper() {
        // Nested function should NOT be instrumented per §12.2
    }
    nested_helper();
}
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analysis should succeed");
    // Only outer_fn should be discovered as candidate
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].function_name, "outer_fn");

    let transformed =
        transform_source_str(source, &report.candidates).expect("transformation should succeed");

    assert!(transformed.contains("/* __cargo_instrument_anchor: \"outer_fn\" */"));
    // nested_helper must NOT have an anchor
    assert!(!transformed.contains("/* __cargo_instrument_anchor: \"nested_helper\" */"));
}

#[test]
fn test_const_fn_exclusion() {
    let source = r#"
pub const fn const_add(a: i32, b: i32) -> i32 {
    a + b
}

pub fn normal_fn() -> i32 {
    42
}
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analysis should succeed");
    // Only normal_fn is a candidate; const_add is excluded
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].function_name, "normal_fn");

    let transformed =
        transform_source_str(source, &report.candidates).expect("transformation should succeed");

    assert!(!transformed.contains("/* __cargo_instrument_anchor: \"const_add\" */"));
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"normal_fn\" */"));
    assert!(transformed.contains("pub const fn const_add(a: i32, b: i32) -> i32 {\n    a + b\n}"));
}

#[test]
fn test_extern_c_exclusion() {
    let source = r#"
pub extern "C" fn ffi_boundary(x: i32) -> i32 {
    x * 2
}

pub fn normal_fn() -> i32 {
    100
}
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analysis should succeed");
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].function_name, "normal_fn");

    let transformed =
        transform_source_str(source, &report.candidates).expect("transformation should succeed");

    assert!(!transformed.contains("/* __cargo_instrument_anchor: \"ffi_boundary\" */"));
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"normal_fn\" */"));
}

#[test]
fn test_self_recursive_exclusion() {
    let source = r#"
pub fn factorial(n: u64) -> u64 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

pub fn non_recursive() -> u64 {
    factorial(5)
}
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analysis should succeed");
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].function_name, "non_recursive");

    let transformed =
        transform_source_str(source, &report.candidates).expect("transformation should succeed");

    assert!(!transformed.contains("/* __cargo_instrument_anchor: \"factorial\" */"));
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"non_recursive\" */"));
}

#[test]
fn test_idempotence_no_double_instrumentation() {
    let source = r#"
pub fn compute() -> i32 {
    let a = 10;
    a + 32
}
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analysis should succeed");
    assert_eq!(report.candidates.len(), 1);

    // Pass 1: transformation
    let transformed_pass1 =
        transform_source_str(source, &report.candidates).expect("pass 1 transformation");

    // Pass 2A: transforming previously transformed source with cached candidate plan
    let transformed_pass2a = transform_source_str(&transformed_pass1, &report.candidates)
        .expect("pass 2a transformation");

    assert_eq!(
        transformed_pass1, transformed_pass2a,
        "Idempotence invariant violated: second transformation pass with cached candidates modified the source"
    );

    // Pass 2B: re-analyzing transformed source from scratch and transforming with fresh candidates
    let report_fresh =
        analyze_source_str("test_crate", Path::new("src/lib.rs"), &transformed_pass1)
            .expect("re-analysis should succeed");
    let transformed_pass2b = transform_source_str(&transformed_pass1, &report_fresh.candidates)
        .expect("pass 2b transformation");

    assert_eq!(
        transformed_pass1, transformed_pass2b,
        "Idempotence invariant violated: second transformation pass with fresh candidates modified the source"
    );

    // Count anchor occurrences: must be exactly 1
    let anchor_count = transformed_pass1.matches(INSTRUMENT_ANCHOR_PREFIX).count();
    assert_eq!(anchor_count, 1, "Expected exactly 1 anchor sentinel");
}

// ============================================================================
// Category C: Defensive Invariants, Fail-Open & Error Handling
// ============================================================================

#[test]
fn test_overlapping_candidate_rejection() {
    let source = "fn foo() { let a = 1; }\n";

    // Fabricate two overlapping candidates
    let c1 = Candidate {
        function_name: "foo_outer".to_string(),
        source_file: PathBuf::from("src/lib.rs"),
        byte_range: 0..23,
        body_byte_range: 9..23,
        kind: FunctionKind::Free,
        is_async: false,
        is_generic: false,
        has_enclosing_generics: false,
        returns_result: false,
        returns_mut_reference: false,
        returns_reference_or_lifetime: false,
    };
    let c2 = Candidate {
        function_name: "foo_inner".to_string(),
        source_file: PathBuf::from("src/lib.rs"),
        byte_range: 12..22,
        body_byte_range: 15..22, // Overlaps with 9..23
        kind: FunctionKind::Free,
        is_async: false,
        is_generic: false,
        has_enclosing_generics: false,
        returns_result: false,
        returns_mut_reference: false,
        returns_reference_or_lifetime: false,
    };

    // H2 Fail-Open: Plan builds, accepts c1, safely skips c2 with OverlappingWithPrevious diagnostic
    let plan = TransformationPlan::build(source, &[c1, c2]).expect("plan should build");
    assert_eq!(
        plan.edits.len(),
        1,
        "First non-overlapping candidate should be accepted"
    );
    assert_eq!(
        plan.skipped.len(),
        1,
        "Overlapping candidate should be safely skipped"
    );
    assert!(matches!(
        plan.skipped[0].reason,
        SkipReason::OverlappingWithPrevious { .. }
    ));
}

#[test]
fn test_permutation_invariance() {
    let source = r#"
fn fn_a() -> i32 { 1 }
fn fn_b() -> i32 { 2 }
fn fn_c() -> i32 { 3 }
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analysis should succeed");
    assert_eq!(report.candidates.len(), 3);

    let candidates_abc = report.candidates.clone();
    let mut candidates_cba = report.candidates.clone();
    candidates_cba.reverse();

    let candidates_bca = vec![
        report.candidates[1].clone(),
        report.candidates[2].clone(),
        report.candidates[0].clone(),
    ];

    let output_abc = transform_source_str(source, &candidates_abc).unwrap();
    let output_cba = transform_source_str(source, &candidates_cba).unwrap();
    let output_bca = transform_source_str(source, &candidates_bca).unwrap();

    assert_eq!(
        output_abc, output_cba,
        "Permutation invariance violated: reverse candidate order produced different output"
    );
    assert_eq!(
        output_abc, output_bca,
        "Permutation invariance violated: rotated candidate order produced different output"
    );
}

#[test]
fn test_invalid_utf8_boundary_rejection() {
    // 🦀 is 4 bytes: 0xF0 0x9F 0xA6 0x80
    let source = "/* 🦀 */ fn test_fn() { 42 }\n";
    let _valid_start = source.find('{').unwrap();
    let valid_end = source.find('}').unwrap() + 1;

    // Fabricate candidate whose range begins in the middle of the crab emoji (byte 4)
    let bad_candidate = Candidate {
        function_name: "bad_utf8_fn".to_string(),
        source_file: PathBuf::from("src/lib.rs"),
        byte_range: 0..valid_end,
        body_byte_range: 4..valid_end, // Byte 4 is mid-codepoint of '🦀'
        kind: FunctionKind::Free,
        is_async: false,
        is_generic: false,
        has_enclosing_generics: false,
        returns_result: false,
        returns_mut_reference: false,
        returns_reference_or_lifetime: false,
    };

    // H2 Fail-Open: Bad candidate is skipped with structured diagnostic
    let plan = TransformationPlan::build(source, &[bad_candidate]).expect("plan should build");
    assert_eq!(plan.edits.len(), 0);
    assert_eq!(plan.skipped.len(), 1);
    assert!(matches!(
        plan.skipped[0].reason,
        SkipReason::InvalidUtf8Boundary { offset: 4 }
    ));
}

#[test]
fn test_in_place_modification_rejected() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let file_path = temp_dir.path().join("source.rs");
    fs::write(&file_path, "fn test() {}\n").expect("write test file");

    let result = transform_source_file(&file_path, &file_path, &[]);
    assert!(matches!(
        result,
        Err(TransformError::InPlaceModificationDisallowed { .. })
    ));
}

#[test]
fn test_original_source_remains_unchanged() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let input_path = temp_dir.path().join("input.rs");
    let output_path = temp_dir.path().join("output.rs");

    let original_content = "// Header\nfn work() -> i32 {\n    123\n}\n";
    fs::write(&input_path, original_content).expect("write input");
    let initial_bytes = fs::read(&input_path).expect("read initial bytes");

    let report = analyze_source_str("test_crate", &input_path, original_content)
        .expect("analyze should succeed");
    transform_source_file(&input_path, &output_path, &report.candidates)
        .expect("transform should succeed");

    let bytes_after = fs::read(&input_path).expect("read bytes after transformation");
    assert_eq!(
        initial_bytes, bytes_after,
        "Invariant violated: original source file on disk was modified!"
    );

    // Output file must be different and contain transformed content
    let output_content = fs::read_to_string(&output_path).expect("read output");
    assert_ne!(original_content, output_content);
    assert!(output_content.contains(INSTRUMENT_ANCHOR_PREFIX));
}

// ============================================================================
// Adversarial Review Findings Regressions: C1, H2, H3, M1, M3
// ============================================================================

#[test]
fn test_c1_cross_file_basename_collision() {
    // Regression test for C1: two distinct files sharing the same basename "handlers.rs"
    // must NEVER cross-pollinate candidates.
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let src_dir = temp_dir.path().join("src");
    let admin_dir = src_dir.join("admin");
    fs::create_dir_all(&admin_dir).expect("create admin dir");

    let user_handlers_path = src_dir.join("handlers.rs");
    let admin_handlers_path = admin_dir.join("handlers.rs");

    // File 1: short user handler
    let user_handlers_src = "pub fn handle_user() -> i32 {\n    10\n}\n";
    fs::write(&user_handlers_path, user_handlers_src).expect("write user handlers");

    // File 2: admin handler with longer body and different candidate
    let admin_handlers_src = "pub fn handle_admin_deeply_nested_operation() -> i32 {\n    let val = 100;\n    val * 2\n}\n";
    fs::write(&admin_handlers_path, admin_handlers_src).expect("write admin handlers");

    // Discover candidates for both files
    let report_user = cargo_instrument::ast::analyze_source_file("test_crate", &user_handlers_path)
        .expect("analyze user handlers");
    let report_admin =
        cargo_instrument::ast::analyze_source_file("test_crate", &admin_handlers_path)
            .expect("analyze admin handlers");

    // Combine all candidates across the crate (as discovery produces)
    let mut combined_candidates = report_user.candidates.clone();
    combined_candidates.extend(report_admin.candidates);
    assert_eq!(combined_candidates.len(), 2);

    // Transform user handlers: must ONLY receive handle_user, never handle_admin
    let out_user = temp_dir.path().join("out_user.rs");
    transform_source_file(&user_handlers_path, &out_user, &combined_candidates)
        .expect("transform user handlers must succeed without cross-file collision");
    let user_res = fs::read_to_string(&out_user).expect("read out_user");
    assert!(user_res.contains("/* __cargo_instrument_anchor: \"handle_user\" */"));
    assert!(!user_res.contains("handle_admin_deeply_nested_operation"));

    // Transform admin handlers: must ONLY receive handle_admin, never handle_user
    let out_admin = temp_dir.path().join("out_admin.rs");
    transform_source_file(&admin_handlers_path, &out_admin, &combined_candidates)
        .expect("transform admin handlers must succeed without cross-file collision");
    let admin_res = fs::read_to_string(&out_admin).expect("read out_admin");
    assert!(admin_res
        .contains("/* __cargo_instrument_anchor: \"handle_admin_deeply_nested_operation\" */"));
    assert!(!admin_res.contains("handle_user"));

    // Verify scoped transformation API directly (C1 improvement)
    let out_scoped = temp_dir.path().join("out_scoped.rs");
    transform_source_file_scoped(&user_handlers_path, &out_scoped, &report_user.candidates)
        .expect("transform_source_file_scoped must succeed");
    let scoped_res = fs::read_to_string(&out_scoped).expect("read out_scoped");
    assert_eq!(user_res, scoped_res);

    assert!(paths_are_identical(
        &user_handlers_path,
        &user_handlers_path
    ));
    assert!(!paths_are_identical(
        &user_handlers_path,
        &admin_handlers_path
    ));
}

#[test]
fn test_h2_missing_opening_brace_skipped() {
    let source = "fn broken() -> i32 { 42 }\n";
    // Candidate pointing to offset 0 (which is 'f', not '{')
    let candidate = Candidate {
        function_name: "broken".to_string(),
        source_file: PathBuf::from("src/lib.rs"),
        byte_range: 0..26,
        body_byte_range: 0..26, // points to 'f'
        kind: FunctionKind::Free,
        is_async: false,
        is_generic: false,
        has_enclosing_generics: false,
        returns_result: false,
        returns_mut_reference: false,
        returns_reference_or_lifetime: false,
    };

    let plan = TransformationPlan::build(source, &[candidate]).expect("plan should build");
    assert_eq!(plan.edits.len(), 0);
    assert_eq!(plan.skipped.len(), 1);
    assert!(matches!(
        plan.skipped[0].reason,
        SkipReason::MissingOpeningBrace {
            offset: 0,
            found: b'f'
        }
    ));
}

#[test]
fn test_h2_fail_open_malformed_candidate_does_not_poison_valid_candidates() {
    let source = r#"
fn valid_one() -> i32 { 1 }

fn valid_two() -> i32 { 2 }
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analyze valid source");
    assert_eq!(report.candidates.len(), 2);

    // Inject a third, malformed candidate with out-of-bounds byte range
    let malformed_candidate = Candidate {
        function_name: "malformed_fn".to_string(),
        source_file: PathBuf::from("src/lib.rs"),
        byte_range: 500..600,
        body_byte_range: 550..600,
        kind: FunctionKind::Free,
        is_async: false,
        is_generic: false,
        has_enclosing_generics: false,
        returns_result: false,
        returns_mut_reference: false,
        returns_reference_or_lifetime: false,
    };

    let mut candidates = report.candidates;
    candidates.push(malformed_candidate);

    // Plan must succeed fail-open: accepts the 2 valid candidates, skips the malformed one
    let plan = TransformationPlan::build(source, &candidates).expect("plan must build fail-open");
    assert_eq!(
        plan.edits.len(),
        2,
        "Both valid candidates must be transformed"
    );
    assert_eq!(plan.skipped.len(), 1, "Malformed candidate must be skipped");
    assert!(matches!(
        plan.skipped[0].reason,
        SkipReason::OutOfBounds { .. }
    ));

    let output = plan.apply(source).expect("apply must succeed");
    assert!(output.contains("/* __cargo_instrument_anchor: \"valid_one\" */"));
    assert!(output.contains("/* __cargo_instrument_anchor: \"valid_two\" */"));
}

#[test]
fn test_h3_emitter_substitution() {
    // Test ADR-006: verify that custom emitter replaces generated text cleanly
    struct CustomTracingEmitter;
    impl Emitter for CustomTracingEmitter {
        fn emit_body_prefix(&self, candidate: &Candidate, line_ending: &str) -> String {
            format!(
                "{line_ending}    let _custom_span = \"trace:{}\";",
                candidate.function_name
            )
        }
    }

    let source = "fn work() -> i32 {\n    42\n}\n";
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analyze should succeed");
    assert_eq!(report.candidates.len(), 1);

    let output =
        transform_source_str_with_emitter(source, &report.candidates, &CustomTracingEmitter)
            .expect("transform with custom emitter");

    assert!(output.contains("let _custom_span = \"trace:work\";"));
    assert!(!output.contains("_cargo_instrument_sentinel"));
}

#[test]
fn test_m1_crlf_preservation() {
    // CRLF source with \r\n
    let crlf_source = "pub fn crlf_target() -> i32 {\r\n    let a = 100;\r\n    a\r\n}\r\n";
    assert_eq!(detect_line_ending(crlf_source), "\r\n");

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), crlf_source)
        .expect("analyze CRLF");
    assert_eq!(report.candidates.len(), 1);

    let transformed =
        transform_source_str(crlf_source, &report.candidates).expect("transform CRLF");

    // Every newline in the transformed output must be preceded by \r
    let bare_lf_count = transformed
        .as_bytes()
        .windows(2)
        .filter(|w| w[1] == b'\n' && w[0] != b'\r')
        .count();
    assert_eq!(
        bare_lf_count, 0,
        "CRLF invariant violated: found bare LF without CR in transformed output"
    );

    // Verify anchor was emitted with CRLF
    assert!(transformed.contains("\r\n    /* __cargo_instrument_anchor: \"crlf_target\" */\r\n"));
}

#[test]
fn test_m3_anchor_in_string_literal_does_not_suppress_instrumentation() {
    // A function containing the anchor string inside a string literal must NOT be falsely skipped
    let source = r#"
pub fn with_anchor_literal() -> &'static str {
    let msg = "/* __cargo_instrument_anchor: \"fake\" */";
    msg
}
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analyze should succeed");
    assert_eq!(report.candidates.len(), 1);

    let transformed =
        transform_source_str(source, &report.candidates).expect("transform should succeed");

    // The genuine sentinel must be inserted
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"with_anchor_literal\" */"));
    assert!(transformed.contains("let _cargo_instrument_sentinel = ();"));
    // The inner string literal must survive untouched
    assert!(transformed.contains("let msg = \"/* __cargo_instrument_anchor: \\\"fake\\\" */\";"));

    // Re-transforming must be idempotent
    let re_transformed =
        transform_source_str(&transformed, &report.candidates).expect("re-transform");
    assert_eq!(transformed, re_transformed);
}

#[test]
fn test_diverging_function() {
    let source = r#"
pub fn diverge(msg: &str) -> ! {
    panic!("fatal: {msg}")
}
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analyze diverging fn");
    assert_eq!(report.candidates.len(), 1);

    let transformed =
        transform_source_str(source, &report.candidates).expect("transform diverging fn");
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"diverge\" */"));

    // Verify it compiles under rustc -D warnings
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let file_path = temp_dir.path().join("diverge.rs");
    fs::write(&file_path, transformed).expect("write diverge.rs");

    let status = Command::new("rustc")
        .arg("--crate-type=lib")
        .arg("--edition=2021")
        .arg("-D")
        .arg("warnings")
        .arg("--out-dir")
        .arg(temp_dir.path())
        .arg(&file_path)
        .status()
        .expect("rustc compile diverging fn");
    assert!(status.success(), "Diverging function must compile cleanly");
}

#[test]
fn test_unsafe_fn() {
    let source = r#"
pub unsafe fn raw_deref(ptr: *const i32) -> i32 {
    *ptr
}
"#;
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analyze unsafe fn");
    assert_eq!(report.candidates.len(), 1);

    let transformed =
        transform_source_str(source, &report.candidates).expect("transform unsafe fn");
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"raw_deref\" */"));

    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let file_path = temp_dir.path().join("unsafe_fn.rs");
    fs::write(&file_path, transformed).expect("write unsafe_fn.rs");

    let status = Command::new("rustc")
        .arg("--crate-type=lib")
        .arg("--edition=2021")
        .arg("-D")
        .arg("warnings")
        .arg("--out-dir")
        .arg(temp_dir.path())
        .arg(&file_path)
        .status()
        .expect("rustc compile unsafe fn");
    assert!(status.success(), "Unsafe function must compile cleanly");
}

#[test]
fn test_empty_body_function() {
    let source = "pub fn no_op() {}\n";
    let report =
        analyze_source_str("test_crate", Path::new("src/lib.rs"), source).expect("analyze empty");
    assert_eq!(report.candidates.len(), 1);

    let transformed = transform_source_str(source, &report.candidates).expect("transform empty fn");
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"no_op\" */"));
    assert!(transformed.contains("let _cargo_instrument_sentinel = ();"));

    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let file_path = temp_dir.path().join("empty_fn.rs");
    fs::write(&file_path, transformed).expect("write empty_fn.rs");

    let status = Command::new("rustc")
        .arg("--crate-type=lib")
        .arg("--edition=2021")
        .arg("-D")
        .arg("warnings")
        .arg("--out-dir")
        .arg(temp_dir.path())
        .arg(&file_path)
        .status()
        .expect("rustc compile empty fn");
    assert!(status.success(), "Empty body function must compile cleanly");
}

// ============================================================================
// Category D: Compilation Proofs (`rustc`, Cargo & Wrapper Pipeline)
// ============================================================================

#[test]
fn test_rustc_compilation_proof_representative_fixtures() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let fixture_path = temp_dir.path().join("fixture.rs");
    let transformed_path = temp_dir.path().join("fixture_transformed.rs");

    let source = r#"
// 🦀 Multi-byte header comments
pub fn free_compute(val: i32) -> i32 {
    val + 10
}

pub async fn async_worker(msg: &str) -> usize {
    msg.len()
}

pub fn generic_runner<T: Clone>(item: T) -> (T, T) {
    (item.clone(), item)
}

pub struct State {
    pub count: i32,
}

impl State {
    pub fn increment(&mut self) -> i32 {
        self.count += 1;
        self.count
    }
}

pub trait Describable {
    fn describe(&self) -> &'static str;
}

impl Describable for State {
    fn describe(&self) -> &'static str {
        "active state"
    }
}
"#;
    fs::write(&fixture_path, source).expect("write fixture");

    let report =
        analyze_source_str("fixture_crate", &fixture_path, source).expect("analyze fixture");
    assert_eq!(report.candidates.len(), 5);

    transform_source_file(&fixture_path, &transformed_path, &report.candidates)
        .expect("transform fixture");

    // Invoke rustc directly on the transformed source with -D warnings
    let status = Command::new("rustc")
        .arg("--crate-type=lib")
        .arg("--edition=2021")
        .arg("-D")
        .arg("warnings")
        .arg("--out-dir")
        .arg(temp_dir.path())
        .arg(&transformed_path)
        .status()
        .expect("failed to execute rustc");

    assert!(
        status.success(),
        "Compilation proof failed: rustc failed to compile transformed source!"
    );
}

#[test]
fn test_cargo_compilation_proof_entire_package() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let project_root = temp_dir.path().join("pkg");
    let src_dir = project_root.join("src");
    fs::create_dir_all(&src_dir).expect("create src dir");

    // 1. Write Cargo.toml
    let cargo_toml = r#"
[package]
name = "proof_pkg"
version = "0.1.0"
edition = "2021"

[dependencies]
"#;
    fs::write(project_root.join("Cargo.toml"), cargo_toml).expect("write Cargo.toml");

    // 2. Write multi-file sources: src/main.rs and src/helpers.rs
    let main_rs = r#"
mod helpers;

fn main() {
    let greeting = helpers::make_greeting("World");
    assert!(!greeting.is_empty());
}
"#;
    let helpers_rs = r#"
pub fn make_greeting(name: &str) -> String {
    format!("Hello, {name}!")
}

#[allow(dead_code)]
pub async fn async_ping() -> &'static str {
    "pong"
}
"#;
    fs::write(src_dir.join("main.rs"), main_rs).expect("write main.rs");
    fs::write(src_dir.join("helpers.rs"), helpers_rs).expect("write helpers.rs");

    // 3. Baseline verification: ensure original crate compiles
    let baseline_check = Command::new("cargo")
        .arg("check")
        .current_dir(&project_root)
        .status()
        .expect("execute baseline cargo check");
    assert!(baseline_check.success(), "baseline check failed");

    // 4. Discover candidates across crate
    let report = cargo_instrument::analyze_source_file("proof_pkg", &src_dir.join("main.rs"))
        .expect("analyze package");
    assert_eq!(report.candidates.len(), 3); // main, make_greeting, async_ping

    // 5. Transform each source file into an isolated instrumented crate directory
    let instrumented_root = temp_dir.path().join("pkg_instrumented");
    let inst_src = instrumented_root.join("src");
    fs::create_dir_all(&inst_src).expect("create instrumented src");
    fs::copy(
        project_root.join("Cargo.toml"),
        instrumented_root.join("Cargo.toml"),
    )
    .expect("copy Cargo.toml");

    transform_source_file(
        &src_dir.join("main.rs"),
        &inst_src.join("main.rs"),
        &report.candidates,
    )
    .expect("transform main.rs");

    transform_source_file(
        &src_dir.join("helpers.rs"),
        &inst_src.join("helpers.rs"),
        &report.candidates,
    )
    .expect("transform helpers.rs");

    // 6. Invoke real Cargo on the transformed package
    let inst_check = Command::new("cargo")
        .arg("check")
        .current_dir(&instrumented_root)
        .status()
        .expect("execute instrumented cargo check");
    assert!(
        inst_check.success(),
        "Cargo compilation proof failed: cargo check failed on transformed package!"
    );

    let inst_build = Command::new("cargo")
        .arg("build")
        .current_dir(&instrumented_root)
        .status()
        .expect("execute instrumented cargo build");
    assert!(
        inst_build.success(),
        "Cargo compilation proof failed: cargo build failed on transformed package!"
    );
}

#[test]
fn test_h1_wrapper_live_pipeline_integration() {
    // H1: Verify the complete live pipeline:
    // Cargo -> RUSTC_WRAPPER -> source discovery -> P1.3 analysis -> P1.4 transform -> mirrored tree -> real rustc
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let project_root = temp_dir.path().join("live_pkg");
    let src_dir = project_root.join("src");
    fs::create_dir_all(&src_dir).expect("create src dir");

    let cargo_toml = r#"
[package]
name = "live_pkg"
version = "0.1.0"
edition = "2021"

[dependencies]
"#;
    fs::write(project_root.join("Cargo.toml"), cargo_toml).expect("write Cargo.toml");

    let main_rs = r#"
fn calculate(x: i32) -> i32 {
    x * 2
}

fn main() {
    let res = calculate(21);
    assert_eq!(res, 42);
}
"#;
    fs::write(src_dir.join("main.rs"), main_rs).expect("write main.rs");

    // Snapshot original source before build
    let initial_main_bytes = fs::read(src_dir.join("main.rs")).expect("read initial main.rs");

    let cargo_instrument_bin = env!("CARGO_BIN_EXE_cargo-instrument");
    let target_dir = project_root.join("target").join("instrumented");

    // Run wrapped cargo build
    let output = Command::new("cargo")
        .arg("build")
        .arg("--target-dir")
        .arg(&target_dir)
        .current_dir(&project_root)
        .env("RUSTC_WRAPPER", cargo_instrument_bin)
        .env("CARGO_INSTRUMENT_WRAPPER_MODE", "1")
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("execute wrapped cargo build");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "Wrapped cargo build failed! stderr:\n{stderr}"
    );

    // 1. Invariant S1/S2: Original source must remain 100% byte-for-byte untouched
    let after_main_bytes = fs::read(src_dir.join("main.rs")).expect("read main.rs after build");
    assert_eq!(
        initial_main_bytes, after_main_bytes,
        "Original source file on disk was modified by the wrapper!"
    );

    // 2. Verify stderr output confirms live mirroring and transformation
    assert!(
        stderr.contains("mirrored and transformed") || stderr.contains("transformed 2 candidates"),
        "Wrapper stderr did not indicate transformation. stderr:\n{stderr}"
    );

    // 3. Verify the mirrored source exists under target/instrumented and contains the sentinel
    let mirrored_sources_dir = target_dir
        .join("debug")
        .join("deps")
        .join("instrumented_sources")
        .join("live_pkg")
        .join("src");

    assert!(
        mirrored_sources_dir.exists(),
        "Mirrored sources directory was not created at {}",
        mirrored_sources_dir.display()
    );

    let mirrored_main =
        fs::read_to_string(mirrored_sources_dir.join("main.rs")).expect("read mirrored main.rs");
    assert!(
        mirrored_main.contains("/* __cargo_instrument_anchor: \"calculate\" */"),
        "Mirrored main.rs did not contain calculate sentinel"
    );
    assert!(
        mirrored_main.contains("let _cargo_instrument_sentinel = ();"),
        "Mirrored main.rs did not contain sentinel statement"
    );
}

#[test]
fn test_cli_transform_subcommand() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let input_path = temp_dir.path().join("sample.rs");
    let output_path = temp_dir.path().join("sample_transformed.rs");

    fs::write(
        &input_path,
        "pub fn add_one(x: i32) -> i32 {\n    x + 1\n}\n",
    )
    .expect("write sample");

    let bin_path = env!("CARGO_BIN_EXE_cargo-instrument");

    let status = Command::new(bin_path)
        .arg("transform")
        .arg(&input_path)
        .arg("--output")
        .arg(&output_path)
        .status()
        .expect("execute cargo-instrument CLI");

    assert!(status.success(), "CLI transform subcommand failed");

    let transformed = fs::read_to_string(&output_path).expect("read transformed output");
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"add_one\" */"));
    assert!(transformed.contains("let _cargo_instrument_sentinel = ();"));
    assert!(transformed.contains("x + 1"));
}

#[test]
fn test_h1_wrapper_absolute_source_path_handling() {
    // Regression test for H1 latent defect:
    // If rustc is invoked with an absolute source path, the mirror path computation
    // must NOT collapse via mirror_base.join(absolute_path) to the original path.
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let project_root = temp_dir.path().join("abs_pkg");
    let src_dir = project_root.join("src");
    fs::create_dir_all(&src_dir).expect("create src dir");

    let cargo_toml = r#"
[package]
name = "abs_pkg"
version = "0.1.0"
edition = "2021"

[dependencies]
"#;
    fs::write(project_root.join("Cargo.toml"), cargo_toml).expect("write Cargo.toml");

    let main_rs = r#"
fn compute_abs(x: i32) -> i32 {
    x + 100
}

fn main() {
    let res = compute_abs(5);
    assert_eq!(res, 105);
}
"#;
    let main_path = src_dir.join("main.rs");
    fs::write(&main_path, main_rs).expect("write main.rs");

    // Canonicalize main_path to guarantee an absolute, normalized path
    let abs_main = main_path.canonicalize().unwrap_or(main_path);

    let cargo_instrument_bin = env!("CARGO_BIN_EXE_cargo-instrument");
    let out_dir = temp_dir.path().join("out");
    fs::create_dir_all(&out_dir).expect("create out dir");

    // Directly invoke cargo-instrument wrapper with an absolute source argument
    let output = Command::new(cargo_instrument_bin)
        .arg("rustc")
        .arg("--crate-name")
        .arg("abs_pkg")
        .arg(&abs_main)
        .arg("--crate-type")
        .arg("bin")
        .arg("--edition")
        .arg("2021")
        .arg("--out-dir")
        .arg(&out_dir)
        .current_dir(&project_root)
        .env("CARGO_INSTRUMENT_WRAPPER_MODE", "1")
        .env("INSTRUMENT_DEBUG", "1")
        .output()
        .expect("execute wrapper with absolute source path");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "Wrapper failed on absolute source path! stderr:\n{stderr}"
    );

    // Verify mirrored source exists under out_dir/instrumented_sources/abs_pkg/
    let mirror_main = out_dir
        .join("instrumented_sources")
        .join("abs_pkg")
        .join("src")
        .join("main.rs");

    assert!(
        mirror_main.exists(),
        "Mirrored main.rs not found at expected path: {}",
        mirror_main.display()
    );

    let mirrored_content = fs::read_to_string(&mirror_main).expect("read mirror");
    assert!(
        mirrored_content.contains("/* __cargo_instrument_anchor: \"compute_abs\" */"),
        "Mirrored content did not contain sentinel"
    );

    // Verify original source remained 100% untouched
    let original_content = fs::read_to_string(&abs_main).expect("read original");
    assert!(
        !original_content.contains("__cargo_instrument_anchor"),
        "Original source was modified in-place!"
    );
}
