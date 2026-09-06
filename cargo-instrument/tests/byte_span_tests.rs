use cargo_instrument::ast::analyze_source_str;
use std::path::Path;

#[test]
fn test_exact_byte_range_ascii() {
    let source = "fn simple() {\n    let a = 10;\n}\n";
    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analysis should succeed");

    assert_eq!(report.candidates.len(), 1);
    let c = &report.candidates[0];

    // Byte slice of function item must match exactly
    let sliced_item = &source[c.byte_range.clone()];
    assert_eq!(sliced_item, "fn simple() {\n    let a = 10;\n}");

    // Byte slice of body block must match exactly { ... }
    let sliced_body = &source[c.body_byte_range.clone()];
    assert_eq!(sliced_body, "{\n    let a = 10;\n}");
    assert!(sliced_body.starts_with('{'));
    assert!(sliced_body.ends_with('}'));
}

#[test]
fn test_utf8_multibyte_before_function() {
    // Explicitly place multi-byte UTF-8 characters BEFORE the target function:
    // 🦀 is 4 bytes: 0xF0 0x9F 0xA6 0x80 (1 char)
    // é is 2 bytes: 0xC3 0xA9 (1 char)
    // ü is 2 bytes: 0xC3 0xBC (1 char)
    // Total prefix has char count != byte count.
    let prefix = "// 🦀 Ferrises and accents: é, ü, ç\n// Another line with 🚀 rocket!\n";
    let fn_str = "pub async fn utf8_target(param: &str) -> usize {\n    param.len()\n}";
    let source = format!("{prefix}{fn_str}\n");

    let prefix_byte_len = prefix.len();
    let prefix_char_count = prefix.chars().count();
    assert_ne!(
        prefix_byte_len, prefix_char_count,
        "Prefix byte length ({}) must differ from char count ({}) to properly test UTF-8 index confusion",
        prefix_byte_len, prefix_char_count
    );

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), &source)
        .expect("analysis should succeed");

    assert_eq!(report.candidates.len(), 1);
    let c = &report.candidates[0];

    // Verify start byte offset is byte-exact, not character-offset
    assert_eq!(c.byte_range.start, prefix_byte_len);

    let sliced = &source[c.byte_range.clone()];
    assert_eq!(
        sliced, fn_str,
        "Slicing source[start..end] with UTF-8 prefix must match the exact function definition"
    );

    let sliced_body = &source[c.body_byte_range.clone()];
    assert_eq!(sliced_body, "{\n    param.len()\n}");
    assert!(sliced_body.starts_with('{'));
    assert!(sliced_body.ends_with('}'));
}

#[test]
fn test_comments_and_unusual_formatting_preservation() {
    let source = r#"
// Leading line comment
/* Multiline
   block comment */

   pub    fn     unusual_spacing  < 'a > (
       x: &'a str,
   ) ->    bool    {
       // Internal comment
       true
   }
"#;

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analysis should succeed");

    assert_eq!(report.candidates.len(), 1);
    let c = &report.candidates[0];

    let sliced = &source[c.byte_range.clone()];
    assert!(sliced.starts_with("pub    fn     unusual_spacing"));
    assert!(sliced.ends_with('}'));

    let body_sliced = &source[c.body_byte_range.clone()];
    assert!(body_sliced.contains("// Internal comment"));
    assert!(body_sliced.starts_with('{'));
    assert!(body_sliced.ends_with('}'));
}

#[test]
fn test_multiline_functions_with_attributes() {
    let source = r#"
/// Documentation comment for calculate
#[must_use]
pub fn calculate(
    a: i32,
    b: i32,
) -> i32 {
    let sum = a + b;
    sum * 2
}
"#;

    let report = analyze_source_str("test_crate", Path::new("src/lib.rs"), source)
        .expect("analysis should succeed");

    assert_eq!(report.candidates.len(), 1);
    let c = &report.candidates[0];

    let sliced = &source[c.byte_range.clone()];
    assert!(sliced.starts_with("/// Documentation comment for calculate"));
    assert!(sliced.ends_with('}'));

    let body_sliced = &source[c.body_byte_range.clone()];
    assert_eq!(body_sliced, "{\n    let sum = a + b;\n    sum * 2\n}");
}
