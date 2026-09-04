use std::fs;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
use syn::visit::Visit;
use thiserror::Error;

use crate::candidate::{Candidate, DiscoveryReport, FunctionKind, UnsafePolicy};

#[derive(Debug, Error)]
pub enum AstError {
    #[error("Failed to read source file '{path}': {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("Failed to parse Rust source in '{path}': {source}")]
    Parse { path: PathBuf, source: syn::Error },
}

/// Analyze a Rust source file and discover all eligible instrumentation candidates.
///
/// Strictly preserves the original source buffer and computes exact byte ranges
/// suitable for later surgical byte-splicing.
pub fn analyze_source_file(
    crate_name: &str,
    source_path: &Path,
) -> Result<DiscoveryReport, AstError> {
    let source_bytes = fs::read(source_path).map_err(|e| AstError::Io {
        path: source_path.to_path_buf(),
        source: e,
    })?;

    let source_text = String::from_utf8(source_bytes).map_err(|e| AstError::Io {
        path: source_path.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
    })?;

    analyze_source_str(crate_name, source_path, &source_text)
}

/// Analyze Rust source text and discover all eligible instrumentation candidates.
pub fn analyze_source_str(
    crate_name: &str,
    source_path: &Path,
    source_text: &str,
) -> Result<DiscoveryReport, AstError> {
    let syn_file = syn::parse_file(source_text).map_err(|e| AstError::Parse {
        path: source_path.to_path_buf(),
        source: e,
    })?;

    // 1. Inspect file-level inner attributes for unsafe policies (R26)
    let unsafe_policy = detect_unsafe_policy(&syn_file.attrs);

    // 2. Visit syntax tree to locate eligible function items
    let mut visitor = CandidateFinder {
        source_path: source_path.to_path_buf(),
        candidates: Vec::new(),
        current_impl: None,
        inside_fn_body: false,
    };

    visitor.visit_file(&syn_file);

    Ok(DiscoveryReport {
        crate_name: crate_name.to_string(),
        source_file: source_path.to_path_buf(),
        unsafe_policy,
        candidates: visitor.candidates,
    })
}

/// Detect whether the file specifies `#![forbid(unsafe_code)]` or `#![deny(unsafe_code)]`.
fn detect_unsafe_policy(attrs: &[syn::Attribute]) -> UnsafePolicy {
    let mut denied = false;
    for attr in attrs {
        if !matches!(attr.style, syn::AttrStyle::Inner(_)) {
            continue;
        }
        if let syn::Meta::List(list) = &attr.meta {
            let is_forbid = list.path.is_ident("forbid");
            let is_deny = list.path.is_ident("deny");
            if is_forbid || is_deny {
                // Check tokens inside forbid(...) or deny(...)
                let tokens_str = list.tokens.to_string();
                if tokens_str.contains("unsafe_code") {
                    if is_forbid {
                        return UnsafePolicy::Forbidden;
                    }
                    if is_deny {
                        denied = true;
                    }
                }
            }
        }
    }
    if denied {
        UnsafePolicy::Denied
    } else {
        UnsafePolicy::Allowed
    }
}

/// Information about an enclosing `impl` block during AST traversal.
struct EnclosingImpl {
    type_name: String,
    trait_name: Option<String>,
    is_generic: bool,
}

/// AST visitor that collects eligible function candidates.
struct CandidateFinder {
    source_path: PathBuf,
    candidates: Vec<Candidate>,
    current_impl: Option<EnclosingImpl>,
    /// Tracks if traversal is currently inside a function body.
    /// Per §12.2 / §16.14, nested functions inside blocks are excluded.
    inside_fn_body: bool,
}

impl<'ast> Visit<'ast> for CandidateFinder {
    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        let self_ty = &i.self_ty;
        let type_name = quote::quote!(#self_ty).to_string();
        let trait_name = i
            .trait_
            .as_ref()
            .map(|(_, path, _)| quote::quote!(#path).to_string());
        let is_generic = !i.generics.params.is_empty();

        let prev = self.current_impl.replace(EnclosingImpl {
            type_name,
            trait_name,
            is_generic,
        });

        syn::visit::visit_item_impl(self, i);

        self.current_impl = prev;
    }

    fn visit_item_fn(&mut self, i: &'ast syn::ItemFn) {
        // If we are already inside a function body, nested functions are excluded per §12.2
        if self.inside_fn_body {
            return;
        }

        // Apply eligibility and exclusion rules
        if !is_eligible_signature(&i.sig) {
            return;
        }

        if has_instrument_attribute(&i.attrs) {
            return;
        }

        if is_directly_self_recursive(&i.sig.ident, &i.block) {
            return;
        }

        if body_has_handwritten_otel(&i.block) {
            return;
        }

        let function_name = i.sig.ident.to_string();
        let byte_range = i.span().byte_range();
        let body_byte_range = i.block.span().byte_range();
        let is_async = i.sig.asyncness.is_some();
        let is_generic = !i.sig.generics.params.is_empty();

        self.candidates.push(Candidate {
            function_name,
            source_file: self.source_path.clone(),
            byte_range,
            body_byte_range,
            kind: FunctionKind::Free,
            is_async,
            is_generic,
            has_enclosing_generics: false,
        });

        // Visit body to allow visiting sub-items (e.g. inner modules or impls, but marking inside_fn_body)
        let prev = self.inside_fn_body;
        self.inside_fn_body = true;
        syn::visit::visit_item_fn(self, i);
        self.inside_fn_body = prev;
    }

    fn visit_impl_item_fn(&mut self, i: &'ast syn::ImplItemFn) {
        if self.inside_fn_body {
            return;
        }

        if !is_eligible_signature(&i.sig) {
            return;
        }

        if has_instrument_attribute(&i.attrs) {
            return;
        }

        if is_directly_self_recursive(&i.sig.ident, &i.block) {
            return;
        }

        if body_has_handwritten_otel(&i.block) {
            return;
        }

        let byte_range = i.span().byte_range();
        let body_byte_range = i.block.span().byte_range();
        let is_async = i.sig.asyncness.is_some();
        let is_generic = !i.sig.generics.params.is_empty();

        let (kind, function_name, has_enclosing_generics) = match &self.current_impl {
            Some(imp) => match &imp.trait_name {
                Some(tr) => (
                    FunctionKind::TraitMethod {
                        trait_name: tr.clone(),
                        type_name: imp.type_name.clone(),
                    },
                    format!("<{} as {}>::{}", imp.type_name, tr, i.sig.ident),
                    imp.is_generic,
                ),
                None => (
                    FunctionKind::InherentMethod {
                        type_name: imp.type_name.clone(),
                    },
                    format!("{}::{}", imp.type_name, i.sig.ident),
                    imp.is_generic,
                ),
            },
            None => (FunctionKind::Free, i.sig.ident.to_string(), false),
        };

        self.candidates.push(Candidate {
            function_name,
            source_file: self.source_path.clone(),
            byte_range,
            body_byte_range,
            kind,
            is_async,
            is_generic,
            has_enclosing_generics,
        });

        let prev = self.inside_fn_body;
        self.inside_fn_body = true;
        syn::visit::visit_impl_item_fn(self, i);
        self.inside_fn_body = prev;
    }
}

/// Check signature exclusions:
/// - Skip `const fn` (would be a compile error to instrument)
/// - Skip `extern "C"` / `unsafe extern`
fn is_eligible_signature(sig: &syn::Signature) -> bool {
    if sig.constness.is_some() {
        return false;
    }
    if sig.abi.is_some() {
        return false;
    }
    true
}

/// Check for existing instrumentation attributes (`#[instrument]`, `#[tracing::instrument]`).
fn has_instrument_attribute(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        let path = attr.path();
        if path.is_ident("instrument") {
            return true;
        }
        if path.segments.len() >= 2
            && path.segments[0].ident == "tracing"
            && path.segments[1].ident == "instrument"
        {
            return true;
        }
    }
    false
}

/// Detect if the function is directly self-recursive (calls itself by name in its body).
fn is_directly_self_recursive(fn_ident: &syn::Ident, block: &syn::Block) -> bool {
    struct RecursionDetector<'a> {
        target: &'a syn::Ident,
        found: bool,
    }

    impl<'ast> Visit<'ast> for RecursionDetector<'_> {
        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            if let syn::Expr::Path(expr_path) = &*call.func {
                if expr_path.path.is_ident(self.target) {
                    self.found = true;
                    return;
                }
            }
            syn::visit::visit_expr_call(self, call);
        }

        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            if call.method == *self.target {
                self.found = true;
                return;
            }
            syn::visit::visit_expr_method_call(self, call);
        }
    }

    let mut detector = RecursionDetector {
        target: fn_ident,
        found: false,
    };
    detector.visit_block(block);
    detector.found
}

/// Detect if the body already contains hand-written OpenTelemetry span creation or attachment.
///
/// Per R10: prevents double-instrumenting functions that manually start spans via
/// `tracer.start(...)`, wrap futures with `.with_context(...)`, or invoke `__otel_` hooks.
fn body_has_handwritten_otel(block: &syn::Block) -> bool {
    struct OtelDetector {
        found: bool,
    }

    impl<'ast> Visit<'ast> for OtelDetector {
        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            if call.method == "with_context" || call.method == "start" {
                self.found = true;
                return;
            }
            syn::visit::visit_expr_method_call(self, call);
        }

        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            if let syn::Expr::Path(expr_path) = &*call.func {
                let s = quote::quote!(#expr_path).to_string();
                if s.contains("__otel_") || s.contains("tracer") {
                    self.found = true;
                    return;
                }
            }
            syn::visit::visit_expr_call(self, call);
        }
    }

    let mut detector = OtelDetector { found: false };
    detector.visit_block(block);
    detector.found
}
