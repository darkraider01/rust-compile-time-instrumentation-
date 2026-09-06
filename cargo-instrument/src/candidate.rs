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

    /// Whether this function returns a mutable reference (e.g. `&mut T` or `Result<&mut T, E>`).
    /// Functions returning mutable references cannot be wrapped in closures (C1) and fall back to prefix-only instrumentation.
    pub returns_mut_reference: bool,

    /// Whether this function returns any reference or explicit lifetime parameter (e.g. `&T`, `&mut T`, `Result<MutName<'_>, E>`).
    /// Functions returning references or lifetimes fall back to prefix-only instrumentation
    /// to prevent `FnMut` closure escape borrow errors on mutable accessors (including aliased `&mut`).
    pub returns_reference_or_lifetime: bool,
}

/// Detailed counts of functions excluded during AST discovery.
///
/// Strictly per-function tally, enabling exact reconciliation:
/// `candidates.len() + skipped_stats.total() == total_functions_in_file`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkippedStats {
    /// Functions annotated with `#[inline]` or `#[inline(always)]`.
    pub inline_attribute: usize,
    /// Functions in standard adapter traits (`Deref`, `DerefMut`, `AsRef`, `AsMut`, `Borrow`, `BorrowMut`).
    pub adapter_trait: usize,
    /// Destructor implementations (`<T as Drop>::drop`).
    pub drop_implementation: usize,
    /// Functions inside `#[cfg(test)]` modules or annotated with `#[cfg(test)]`/`#[test]`.
    pub cfg_test: usize,
    /// Compile-time constant functions (`const fn`).
    pub const_fn: usize,
    /// Functions with foreign or `extern "C"` ABI.
    pub extern_abi: usize,
    /// Directly self-recursive functions (R7).
    pub self_recursive: usize,
    /// Functions with handwritten OpenTelemetry calls or `#[instrument]` attributes (R10).
    pub handwritten_otel: usize,
    /// Nested functions inside another function's body block (§12.2 / §16.14).
    pub nested_function: usize,
}

impl SkippedStats {
    /// Sum of all skipped function counts across all categories.
    pub fn total(&self) -> usize {
        self.inline_attribute
            + self.adapter_trait
            + self.drop_implementation
            + self.cfg_test
            + self.const_fn
            + self.extern_abi
            + self.self_recursive
            + self.handwritten_otel
            + self.nested_function
    }
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
    /// Whether the file specifies `#![no_std]` or `#![cfg_attr(..., no_std)]` (§12.3).
    pub is_no_std: bool,
    /// Whether the crate exports any C-ABI symbols colliding with `otel-shim`.
    pub has_colliding_symbols: bool,
    /// Per-function statistics of excluded functions.
    pub skipped_stats: SkippedStats,
    /// Discovered eligible function candidates.
    pub candidates: Vec<Candidate>,
}

impl DiscoveryReport {
    /// Format this report in development/debug mode.
    ///
    /// Produces human-readable output indicating the crate, source file,
    /// unsafe policy, no_std status, colliding symbol status, skipped stats,
    /// and candidate byte ranges.
    pub fn format_debug(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("crate: {}\n", self.crate_name));
        out.push_str(&format!("source: {}\n", self.source_file.display()));
        out.push_str(&format!("unsafe_policy: {:?}\n", self.unsafe_policy));
        out.push_str(&format!("is_no_std: {}\n", self.is_no_std));
        out.push_str(&format!(
            "has_colliding_symbols: {}\n",
            self.has_colliding_symbols
        ));
        out.push_str(&format!("skipped_stats: {:?}\n\n", self.skipped_stats));
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
