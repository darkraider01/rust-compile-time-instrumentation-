use std::ops::Range;
use std::path::PathBuf;

/// Classification of the file-level unsafe code lint policy.
///
/// Distinguishes between `forbid` and `deny` per Phase 0 / R26:
/// `#![forbid(unsafe_code)]` cannot be lifted by an inner `#[allow]`, while
/// `#![deny(unsafe_code)]` can be locally permitted with scoped allowances.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsafePolicy {
    /// Neither `#![forbid(unsafe_code)]` nor `#![deny(unsafe_code)]` was detected at the file level.
    Allowed,
    /// `#![deny(unsafe_code)]` was detected.
    Denied,
    /// `#![forbid(unsafe_code)]` was detected.
    Forbidden,
}

/// Category of an eligible function definition.
///
/// Strictly conforms to Phase 0 §12.2 supported constructs:
/// Free functions, Inherent methods, Trait impl methods.
/// Nested functions are deliberately excluded per §12.2 / §16.14.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionKind {
    /// Free function defined at crate or module level.
    Free,
    /// Method defined in an inherent impl block (`impl Foo { ... }`).
    InherentMethod {
        /// String representation of the type implementing this method.
        type_name: String,
    },
    /// Method defined in a trait implementation block (`impl Trait for Foo { ... }`).
    TraitMethod {
        /// String representation of the trait being implemented.
        trait_name: String,
        /// String representation of the implementing type.
        type_name: String,
    },
}

/// An identified function candidate for compile-time instrumentation.
///
/// Refers to exact byte ranges in the ORIGINAL source file buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Fully-qualified or contextual function name (e.g. `my_fn`, `Foo::bar`, `<Foo as Trait>::baz`).
    pub function_name: String,

    /// Path to the source file where this candidate was discovered.
    pub source_file: PathBuf,

    /// Exact byte range `start..end` of the entire function definition item in the original UTF-8 buffer.
    /// Guaranteed to be suitable for `&source[start..end]`.
    pub byte_range: Range<usize>,

    /// Exact byte range `start..end` of the function body block `{ ... }` in the original UTF-8 buffer.
    pub body_byte_range: Range<usize>,

    /// Category of function definition.
    pub kind: FunctionKind,

    /// Whether this function is marked `async`.
    pub is_async: bool,

    /// Whether the function definition itself carries generic parameters (type, lifetime, or const params).
    ///
    /// Note: Does not reflect whether an enclosing `impl<T>` is generic.
    /// See `has_enclosing_generics` for the enclosing impl state.
    pub is_generic: bool,

    /// Whether this function resides within a generic `impl<...>` block.
    pub has_enclosing_generics: bool,

    /// Whether this function returns `Result<T, E>`.
    pub returns_result: bool,
}

/// Summary report of AST candidate discovery for a single compilation unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryReport {
    /// Name of the crate being compiled.
    pub crate_name: String,
    /// Path to the root source file analyzed.
    pub source_file: PathBuf,
    /// File-level unsafe lint policy.
    pub unsafe_policy: UnsafePolicy,
    /// Discovered eligible function candidates.
    pub candidates: Vec<Candidate>,
}

impl DiscoveryReport {
    /// Format this report in development/debug mode.
    ///
    /// Produces human-readable output indicating the crate, source file,
    /// unsafe policy, and candidate byte ranges.
    pub fn format_debug(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("crate: {}\n", self.crate_name));
        out.push_str(&format!("source: {}\n", self.source_file.display()));
        out.push_str(&format!("unsafe_policy: {:?}\n\n", self.unsafe_policy));
        out.push_str("candidates:\n");
        if self.candidates.is_empty() {
            out.push_str("  (none)\n");
        } else {
            for c in &self.candidates {
                out.push_str(&format!(
                    "  - {}: bytes {}..{} ({})\n",
                    c.function_name,
                    c.byte_range.start,
                    c.byte_range.end,
                    c.source_file.display()
                ));
            }
        }
        out
    }
}
