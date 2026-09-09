use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
use syn::visit::Visit;
use thiserror::Error;

use crate::candidate::{Candidate, DiscoveryReport, FunctionKind, SkippedStats, UnsafePolicy};

/// The seven runtime C-ABI symbols exported by `otel-shim`.
pub const OTEL_ABI_SYMBOLS: &[&str] = &[
    "__otel_span_enter",
    "__otel_span_exit",
    "__otel_span_set_error",
    "__otel_span_start",
    "__otel_span_end",
    "__otel_ctx_attach",
    "__otel_ctx_detach",
];

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

/// Analyze a Rust crate's root source file and discover all eligible instrumentation candidates
/// across the primary file and any referenced submodules.
///
/// Strictly preserves the original source buffer and computes exact byte ranges
/// suitable for later surgical byte-splicing.
pub fn analyze_source_file(
    crate_name: &str,
    root_path: &Path,
) -> Result<DiscoveryReport, AstError> {
    let source_bytes = fs::read(root_path).map_err(|e| AstError::Io {
        path: root_path.to_path_buf(),
        source: e,
    })?;

    let source_text = String::from_utf8(source_bytes).map_err(|e| AstError::Io {
        path: root_path.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
    })?;

    let root_syn = syn::parse_file(&source_text).map_err(|e| AstError::Parse {
        path: root_path.to_path_buf(),
        source: e,
    })?;

    // 1. Inspect root file-level inner attributes for unsafe policies (R26) and no_std (§12.3)
    let unsafe_policy = detect_unsafe_policy(&root_syn.attrs);
    let is_no_std = detect_no_std(&root_syn.attrs);

    // 2. Discover candidates across the root file and recursively in submodules
    let mut visited = HashSet::new();
    let mut candidates = Vec::new();
    let mut skipped_stats = SkippedStats::default();
    let mut has_colliding_symbols = false;

    analyze_file_and_submodules(
        root_path,
        &root_syn,
        /* is_root: */ true,
        &mut visited,
        &mut candidates,
        &mut skipped_stats,
        &mut has_colliding_symbols,
    );

    Ok(DiscoveryReport {
        crate_name: crate_name.to_string(),
        source_file: root_path.to_path_buf(),
        unsafe_policy,
        is_no_std,
        has_colliding_symbols,
        skipped_stats,
        candidates,
    })
}

/// Analyze Rust source text and discover all eligible instrumentation candidates.
/// Primarily used for in-memory analysis and unit tests.
pub fn analyze_source_str(
    crate_name: &str,
    source_path: &Path,
    source_text: &str,
) -> Result<DiscoveryReport, AstError> {
    let syn_file = syn::parse_file(source_text).map_err(|e| AstError::Parse {
        path: source_path.to_path_buf(),
        source: e,
    })?;

    let unsafe_policy = detect_unsafe_policy(&syn_file.attrs);
    let is_no_std = detect_no_std(&syn_file.attrs);

    let mut visited = HashSet::new();
    let mut candidates = Vec::new();
    let mut skipped_stats = SkippedStats::default();
    let mut has_colliding_symbols = false;

    analyze_file_and_submodules(
        source_path,
        &syn_file,
        /* is_root: */ true,
        &mut visited,
        &mut candidates,
        &mut skipped_stats,
        &mut has_colliding_symbols,
    );

    Ok(DiscoveryReport {
        crate_name: crate_name.to_string(),
        source_file: source_path.to_path_buf(),
        unsafe_policy,
        is_no_std,
        has_colliding_symbols,
        skipped_stats,
        candidates,
    })
}

/// Recursively analyze a file's AST items and discover child submodules.
///
/// Threading `is_root: bool` ensures module resolution follows Rust's crate root rules:
/// for crate roots (e.g. `src/main.rs`, `src/lib.rs`, `src/bin/tool.rs`, `tests/integ.rs`),
/// child modules are siblings in `current_file.parent()`. For non-root files,
/// child modules resolve under `current_file.parent().join(file_stem)` (or parent if `mod.rs`).
fn analyze_file_and_submodules(
    file_path: &Path,
    syn_file: &syn::File,
    is_root: bool,
    visited: &mut HashSet<PathBuf>,
    candidates: &mut Vec<Candidate>,
    skipped_stats: &mut SkippedStats,
    has_colliding_symbols: &mut bool,
) {
    let canonical = file_path
        .canonicalize()
        .unwrap_or_else(|_| file_path.to_path_buf());
    if !visited.insert(canonical) {
        return;
    }

    // 1. Determine base directory for resolving submodules declared directly in this file
    let base_dir = if is_root || file_path.file_name().and_then(|s| s.to_str()) == Some("mod.rs") {
        file_path.parent().unwrap_or(Path::new(".")).to_path_buf()
    } else {
        let stem = file_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        file_path.parent().unwrap_or(Path::new(".")).join(stem)
    };

    // 2. Visit syntax tree of this file
    let file_has_otel = file_has_otel_import(syn_file);
    let mut visitor = CandidateFinder {
        source_path: file_path.to_path_buf(),
        current_dir: base_dir.clone(),
        candidates: Vec::new(),
        current_impl: None,
        inside_fn_body: false,
        file_has_otel,
        skipped_stats: SkippedStats::default(),
        has_colliding_symbols: false,
    };
    visitor.visit_file(syn_file);
    candidates.extend(visitor.candidates);

    skipped_stats.inline_attribute += visitor.skipped_stats.inline_attribute;
    skipped_stats.adapter_trait += visitor.skipped_stats.adapter_trait;
    skipped_stats.drop_implementation += visitor.skipped_stats.drop_implementation;
    skipped_stats.cfg_test += visitor.skipped_stats.cfg_test;
    skipped_stats.const_fn += visitor.skipped_stats.const_fn;
    skipped_stats.extern_abi += visitor.skipped_stats.extern_abi;
    skipped_stats.self_recursive += visitor.skipped_stats.self_recursive;
    skipped_stats.handwritten_otel += visitor.skipped_stats.handwritten_otel;
    skipped_stats.nested_function += visitor.skipped_stats.nested_function;
    if visitor.has_colliding_symbols {
        *has_colliding_symbols = true;
    }

    // 3. Recursively discover and analyze submodules
    discover_submodules(
        file_path,
        &syn_file.items,
        &base_dir,
        visited,
        candidates,
        skipped_stats,
        has_colliding_symbols,
    );
}

/// Discover submodules declared in an item list (either in a file or inside an inline module).
fn discover_submodules(
    current_file: &Path,
    items: &[syn::Item],
    current_dir: &Path,
    visited: &mut HashSet<PathBuf>,
    candidates: &mut Vec<Candidate>,
    skipped_stats: &mut SkippedStats,
    has_colliding_symbols: &mut bool,
) {
    for item in items {
        if let syn::Item::Mod(item_mod) = item {
            // Skip submodules marked #[cfg(test)] (their functions were already tallied by CandidateFinder)
            if is_cfg_test(&item_mod.attrs) {
                continue;
            }

            if let Some((_, inner_items)) = &item_mod.content {
                // Inline module: mod foo { ... }
                let sub_dir = if let Some(custom_path) = extract_path_attribute(&item_mod.attrs) {
                    current_file
                        .parent()
                        .unwrap_or(Path::new("."))
                        .join(custom_path)
                } else {
                    current_dir.join(item_mod.ident.to_string())
                };
                discover_submodules(
                    current_file,
                    inner_items,
                    &sub_dir,
                    visited,
                    candidates,
                    skipped_stats,
                    has_colliding_symbols,
                );
            } else {
                // Out-of-line module: mod foo;
                let submod_path = resolve_submodule_path(current_file, current_dir, item_mod);

                if let Some(target_file) = submod_path {
                    match fs::read(&target_file) {
                        Ok(bytes) => match String::from_utf8(bytes) {
                            Ok(text) => match syn::parse_file(&text) {
                                Ok(sub_syn) => {
                                    analyze_file_and_submodules(
                                        &target_file,
                                        &sub_syn,
                                        /* is_root: */ false,
                                        visited,
                                        candidates,
                                        skipped_stats,
                                        has_colliding_symbols,
                                    );
                                }
                                Err(e) => {
                                    eprintln!(
                                        "warning: cargo-instrument: failed to parse submodule '{}': {e}",
                                        target_file.display()
                                    );
                                }
                            },
                            Err(e) => {
                                eprintln!(
                                    "warning: cargo-instrument: submodule '{}' is not valid UTF-8: {e}",
                                    target_file.display()
                                );
                            }
                        },
                        Err(e) => {
                            eprintln!(
                                "warning: cargo-instrument: failed to read submodule '{}': {e}",
                                target_file.display()
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Resolve the file path of an out-of-line submodule declaration (`mod foo;`).
fn resolve_submodule_path(
    current_file: &Path,
    current_dir: &Path,
    item_mod: &syn::ItemMod,
) -> Option<PathBuf> {
    if let Some(custom_path) = extract_path_attribute(&item_mod.attrs) {
        let p = current_file
            .parent()
            .unwrap_or(Path::new("."))
            .join(custom_path);
        if p.exists() {
            Some(p)
        } else {
            None
        }
    } else {
        let submod_name = item_mod.ident.to_string();
        let candidate1 = current_dir.join(format!("{}.rs", submod_name));
        let candidate2 = current_dir.join(&submod_name).join("mod.rs");
        if candidate1.exists() {
            Some(candidate1)
        } else if candidate2.exists() {
            Some(candidate2)
        } else {
            None
        }
    }
}

/// Extract `#[path = "..."]` attribute string if present on an item.
fn extract_path_attribute(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        if attr.path().is_ident("path") {
            if let syn::Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) = &nv.value
                {
                    return Some(s.value());
                }
            }
        }
    }
    None
}

/// Verify that an application crate declaring `otel-shim` contains a genuine Rust item path
/// into `otel_shim` (e.g. `otel_shim::init()`), preventing rustc from pruning `libotel_shim.rlib`
/// from the linker command line (ADR-003 / E-10).
///
/// Recursively inspects the entire module tree starting at `root_path`.
pub fn check_application_preflight(crate_name: &str, root_path: &Path) -> Result<(), String> {
    let source_bytes = fs::read(root_path).map_err(|e| {
        format!(
            "Failed to read application root source '{path}': {e}",
            path = root_path.display()
        )
    })?;

    let source_text = String::from_utf8(source_bytes).map_err(|e| {
        format!(
            "Application root source '{path}' is not valid UTF-8: {e}",
            path = root_path.display()
        )
    })?;

    let root_syn = syn::parse_file(&source_text).map_err(|e| {
        format!(
            "Failed to parse application root source '{path}': {e}",
            path = root_path.display()
        )
    })?;

    let mut visited = HashSet::new();
    let found = check_file_and_submodules_for_otel_shim(root_path, &root_syn, true, &mut visited);

    if found {
        Ok(())
    } else {
        Err(format!(
            "cargo-instrument: application crate '{crate_name}' must reference `otel_shim` \
            (e.g. `otel_shim::init()`) to prevent rustc extern-crate pruning when dependencies \
            are instrumented (ADR-003 / E-10). Add `otel_shim::init();` in '{path}' or a child module.",
            path = root_path.display()
        ))
    }
}

/// Check whether `otel_shim` is referenced in this file or any recursively declared submodules.
fn check_file_and_submodules_for_otel_shim(
    file_path: &Path,
    syn_file: &syn::File,
    is_root: bool,
    visited: &mut HashSet<PathBuf>,
) -> bool {
    let canonical = file_path
        .canonicalize()
        .unwrap_or_else(|_| file_path.to_path_buf());
    if !visited.insert(canonical) {
        return false;
    }

    let mut visitor = OtelShimVisitor { found: false };
    visitor.visit_file(syn_file);
    if visitor.found {
        return true;
    }

    let base_dir = if is_root || file_path.file_name().and_then(|s| s.to_str()) == Some("mod.rs") {
        file_path.parent().unwrap_or(Path::new(".")).to_path_buf()
    } else {
        let stem = file_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        file_path.parent().unwrap_or(Path::new(".")).join(stem)
    };

    discover_submodules_for_otel_shim(file_path, &syn_file.items, &base_dir, visited)
}

fn discover_submodules_for_otel_shim(
    current_file: &Path,
    items: &[syn::Item],
    current_dir: &Path,
    visited: &mut HashSet<PathBuf>,
) -> bool {
    for item in items {
        if let syn::Item::Mod(item_mod) = item {
            if let Some((_, inner_items)) = &item_mod.content {
                let sub_dir = if let Some(custom_path) = extract_path_attribute(&item_mod.attrs) {
                    current_file
                        .parent()
                        .unwrap_or(Path::new("."))
                        .join(custom_path)
                } else {
                    current_dir.join(item_mod.ident.to_string())
                };
                if discover_submodules_for_otel_shim(current_file, inner_items, &sub_dir, visited) {
                    return true;
                }
            } else {
                let submod_path = resolve_submodule_path(current_file, current_dir, item_mod);
                if let Some(target_file) = submod_path {
                    if let Ok(bytes) = fs::read(&target_file) {
                        if let Ok(text) = String::from_utf8(bytes) {
                            if let Ok(sub_syn) = syn::parse_file(&text) {
                                if check_file_and_submodules_for_otel_shim(
                                    &target_file,
                                    &sub_syn,
                                    /* is_root: */ false,
                                    visited,
                                ) {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    false
}

/// AST visitor that checks whether `otel_shim` is referenced anywhere in the syntax tree.
struct OtelShimVisitor {
    found: bool,
}

impl<'ast> Visit<'ast> for OtelShimVisitor {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        if let Some(first) = path.segments.first() {
            if first.ident == "otel_shim" {
                self.found = true;
                return;
            }
        }
        syn::visit::visit_path(self, path);
    }

    fn visit_item_extern_crate(&mut self, item: &'ast syn::ItemExternCrate) {
        if item.ident == "otel_shim" {
            self.found = true;
            return;
        }
        syn::visit::visit_item_extern_crate(self, item);
    }

    fn visit_use_tree(&mut self, tree: &'ast syn::UseTree) {
        match tree {
            syn::UseTree::Path(use_path) => {
                if use_path.ident == "otel_shim" {
                    self.found = true;
                    return;
                }
                self.visit_use_tree(&use_path.tree);
            }
            syn::UseTree::Name(use_name) => {
                if use_name.ident == "otel_shim" {
                    self.found = true;
                }
            }
            syn::UseTree::Rename(use_rename) => {
                if use_rename.ident == "otel_shim" {
                    self.found = true;
                }
            }
            syn::UseTree::Glob(_) => {}
            syn::UseTree::Group(use_group) => {
                for item in &use_group.items {
                    self.visit_use_tree(item);
                }
            }
        }
    }
}

/// Check if the file imports OpenTelemetry types or tracing utilities.
fn file_has_otel_import(file: &syn::File) -> bool {
    struct UseFinder {
        has_otel: bool,
    }

    impl<'ast> Visit<'ast> for UseFinder {
        fn visit_item_use(&mut self, item_use: &'ast syn::ItemUse) {
            let use_str = quote::quote!(#item_use).to_string();
            if use_str.contains("opentelemetry") || use_str.contains("Tracer") {
                self.has_otel = true;
            }
            syn::visit::visit_item_use(self, item_use);
        }
    }

    let mut finder = UseFinder { has_otel: false };
    finder.visit_file(file);
    finder.has_otel
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

/// Detect whether the file specifies `#![no_std]` or `#![cfg_attr(..., no_std)]`.
pub fn detect_no_std(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if !matches!(attr.style, syn::AttrStyle::Inner(_)) {
            continue;
        }
        match &attr.meta {
            syn::Meta::Path(p) => {
                if p.is_ident("no_std") {
                    return true;
                }
            }
            syn::Meta::List(list) if list.path.is_ident("cfg_attr") => {
                let tokens_str = list.tokens.to_string();
                if tokens_str.split(',').any(|part| part.trim() == "no_std") {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Check if the function has `#[inline]` or `#[inline(always)]`.
///
/// NOTE (M2): `#[inline(never)]` is explicitly kept eligible because it designates
/// an out-of-line call boundary that is an ideal candidate for instrumentation.
fn has_inline_attribute(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if attr.path().is_ident("inline") {
            match &attr.meta {
                syn::Meta::Path(_) => return true,
                syn::Meta::List(list) => {
                    let tokens_str = list.tokens.to_string();
                    if tokens_str.contains("never") {
                        continue;
                    }
                    return true;
                }
                _ => {}
            }
        }
    }
    false
}

/// Check if an item is gated behind `#[cfg(test)]` or marked `#[test]`.
fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if attr.path().is_ident("test") {
            return true;
        }
        if attr.path().is_ident("cfg") {
            if let syn::Meta::List(list) = &attr.meta {
                let tokens_str = list.tokens.to_string();
                if tokens_str.contains("test") {
                    return true;
                }
            }
        }
    }
    false
}

/// Check if a function is annotated with `#[no_mangle]` or `#[export_name]`
/// matching any of the 7 runtime C-ABI symbols in `OTEL_ABI_SYMBOLS`.
fn is_colliding_symbol(ident: &syn::Ident, attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if attr.path().is_ident("no_mangle") {
            let ident_str = ident.to_string();
            if OTEL_ABI_SYMBOLS.contains(&ident_str.as_str()) {
                return true;
            }
        }
        if attr.path().is_ident("export_name") {
            if let syn::Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) = &nv.value
                {
                    if OTEL_ABI_SYMBOLS.contains(&s.value().as_str()) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// AST visitor to count total function definitions across a syntax tree.
struct FnCounter {
    count: usize,
}

impl<'ast> Visit<'ast> for FnCounter {
    fn visit_item_fn(&mut self, i: &'ast syn::ItemFn) {
        self.count += 1;
        syn::visit::visit_item_fn(self, i);
    }

    fn visit_impl_item_fn(&mut self, i: &'ast syn::ImplItemFn) {
        self.count += 1;
        syn::visit::visit_impl_item_fn(self, i);
    }
}

/// Count total function definitions (free functions, inherent/trait methods,
/// and nested functions) in a syn File.
pub fn count_total_functions(file: &syn::File) -> usize {
    let mut counter = FnCounter { count: 0 };
    counter.visit_file(file);
    counter.count
}

/// Count all function definitions across a crate root and its out-of-line submodules.
pub fn count_crate_functions(root_path: &Path, root_syn: &syn::File) -> usize {
    let mut visited = HashSet::new();
    let mut total = 0;
    count_file_and_submodules_helper(root_path, root_syn, true, &mut visited, &mut total);
    total
}

fn count_file_and_submodules_helper(
    file_path: &Path,
    syn_file: &syn::File,
    is_root: bool,
    visited: &mut HashSet<PathBuf>,
    total: &mut usize,
) {
    let canonical = file_path
        .canonicalize()
        .unwrap_or_else(|_| file_path.to_path_buf());
    if !visited.insert(canonical) {
        return;
    }

    *total += count_total_functions(syn_file);

    let base_dir = if is_root || file_path.file_name().and_then(|s| s.to_str()) == Some("mod.rs") {
        file_path.parent().unwrap_or(Path::new(".")).to_path_buf()
    } else {
        let stem = file_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        file_path.parent().unwrap_or(Path::new(".")).join(stem)
    };

    for item in &syn_file.items {
        if let syn::Item::Mod(item_mod) = item {
            if item_mod.content.is_none() {
                if let Some(target_file) = resolve_submodule_path(file_path, &base_dir, item_mod) {
                    if let Ok(bytes) = fs::read(&target_file) {
                        if let Ok(text) = String::from_utf8(bytes) {
                            if let Ok(child_syn) = syn::parse_file(&text) {
                                count_file_and_submodules_helper(
                                    &target_file,
                                    &child_syn,
                                    false,
                                    visited,
                                    total,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Count all functions declared inside a module (inline items or out-of-line submodule).
fn count_all_functions_in_mod(m: &syn::ItemMod, current_file: &Path, current_dir: &Path) -> usize {
    if let Some((_, items)) = &m.content {
        let mut counter = FnCounter { count: 0 };
        for it in items {
            counter.visit_item(it);
        }
        counter.count
    } else if let Some(target_file) = resolve_submodule_path(current_file, current_dir, m) {
        if let Ok(bytes) = fs::read(&target_file) {
            if let Ok(text) = String::from_utf8(bytes) {
                if let Ok(file) = syn::parse_file(&text) {
                    return count_total_functions(&file);
                }
            }
        }
        0
    } else {
        0
    }
}

/// Normalize token-stream-to-string representation of types and paths.
///
/// `quote!(#node).to_string()` inserts spaces between punctuation tokens (e.g.
/// `From < std :: num :: ParseIntError >`). This function collapses those stray
/// spaces around `::`, `<`, `>`, and `>>` while preserving intentional spaces
/// like `" as "`.
pub(crate) fn normalize_type_str(s: &str) -> String {
    let mut out = s.trim().to_string();
    let mut prev = String::new();
    while prev != out {
        prev = out.clone();
        out = out
            .replace(" :: ", "::")
            .replace(":: ", "::")
            .replace(" ::", "::")
            .replace(" < ", "<")
            .replace(" <", "<")
            .replace("< ", "<")
            .replace(" >>", ">>")
            .replace(" >", ">")
            .replace(" , ", ", ")
            .replace(" ,", ",");
    }
    out
}

/// Information about an enclosing `impl` block during AST traversal.
struct EnclosingImpl {
    type_name: String,
    trait_name: Option<String>,
    trait_ident: Option<String>,
    is_generic: bool,
}

/// AST visitor that collects eligible function candidates.
struct CandidateFinder {
    source_path: PathBuf,
    current_dir: PathBuf,
    candidates: Vec<Candidate>,
    current_impl: Option<EnclosingImpl>,
    /// Tracks if traversal is currently inside a function body.
    /// Per §12.2 / §16.14, nested functions inside blocks are excluded.
    inside_fn_body: bool,
    file_has_otel: bool,
    skipped_stats: SkippedStats,
    has_colliding_symbols: bool,
}

impl<'ast> CandidateFinder {
    fn visit_skipped_fn_body(&mut self, block: &'ast syn::Block) {
        let prev = self.inside_fn_body;
        self.inside_fn_body = true;
        syn::visit::visit_block(self, block);
        self.inside_fn_body = prev;
    }
}

impl<'ast> Visit<'ast> for CandidateFinder {
    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        let self_ty = &i.self_ty;
        let type_name = normalize_type_str(&quote::quote!(#self_ty).to_string());
        let trait_name = i
            .trait_
            .as_ref()
            .map(|(_, path, _)| normalize_type_str(&quote::quote!(#path).to_string()));
        let trait_ident = i
            .trait_
            .as_ref()
            .and_then(|(_, path, _)| path.segments.last().map(|s| s.ident.to_string()));
        let is_generic = !i.generics.params.is_empty();

        let prev = self.current_impl.replace(EnclosingImpl {
            type_name,
            trait_name,
            trait_ident,
            is_generic,
        });

        syn::visit::visit_item_impl(self, i);

        self.current_impl = prev;
    }

    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        if is_cfg_test(&m.attrs) {
            self.skipped_stats.cfg_test +=
                count_all_functions_in_mod(m, &self.source_path, &self.current_dir);
            return;
        }
        syn::visit::visit_item_mod(self, m);
    }

    fn visit_foreign_item_fn(&mut self, i: &'ast syn::ForeignItemFn) {
        if is_colliding_symbol(&i.sig.ident, &i.attrs) {
            self.has_colliding_symbols = true;
        }
        syn::visit::visit_foreign_item_fn(self, i);
    }

    fn visit_item_fn(&mut self, i: &'ast syn::ItemFn) {
        if is_colliding_symbol(&i.sig.ident, &i.attrs) {
            self.has_colliding_symbols = true;
        }

        // If we are already inside a function body, nested functions are excluded per §12.2 / §16.14
        if self.inside_fn_body {
            self.skipped_stats.nested_function += 1;
            return;
        }

        if is_cfg_test(&i.attrs) {
            self.skipped_stats.cfg_test += 1;
            self.visit_skipped_fn_body(&i.block);
            return;
        }

        if i.sig.constness.is_some() {
            self.skipped_stats.const_fn += 1;
            self.visit_skipped_fn_body(&i.block);
            return;
        }

        if i.sig.abi.is_some() {
            self.skipped_stats.extern_abi += 1;
            self.visit_skipped_fn_body(&i.block);
            return;
        }

        if has_inline_attribute(&i.attrs) {
            self.skipped_stats.inline_attribute += 1;
            self.visit_skipped_fn_body(&i.block);
            return;
        }

        if has_instrument_attribute(&i.attrs) {
            self.skipped_stats.handwritten_otel += 1;
            self.visit_skipped_fn_body(&i.block);
            return;
        }

        if is_directly_self_recursive(&i.sig.ident, &i.block) {
            self.skipped_stats.self_recursive += 1;
            self.visit_skipped_fn_body(&i.block);
            return;
        }

        if body_has_handwritten_otel(&i.block, self.file_has_otel) {
            self.skipped_stats.handwritten_otel += 1;
            self.visit_skipped_fn_body(&i.block);
            return;
        }

        let function_name = i.sig.ident.to_string();
        let byte_range = i.span().byte_range();
        let body_byte_range = i.block.span().byte_range();
        let is_async = i.sig.asyncness.is_some();
        let is_generic = !i.sig.generics.params.is_empty();
        let returns_result = is_result_return_type(&i.sig.output);
        let returns_mut_reference = returns_mut_reference(&i.sig.output);
        let returns_reference_or_lifetime = returns_reference_or_lifetime(&i.sig.output);

        self.candidates.push(Candidate {
            function_name,
            source_file: self.source_path.clone(),
            byte_range,
            body_byte_range,
            kind: FunctionKind::Free,
            is_async,
            is_generic,
            has_enclosing_generics: false,
            returns_result,
            returns_mut_reference,
            returns_reference_or_lifetime,
        });

        // Visit body to allow visiting sub-items (e.g. inner modules or impls, but marking inside_fn_body)
        let prev = self.inside_fn_body;
        self.inside_fn_body = true;
        syn::visit::visit_item_fn(self, i);
        self.inside_fn_body = prev;
    }

    fn visit_impl_item_fn(&mut self, i: &'ast syn::ImplItemFn) {
        if is_colliding_symbol(&i.sig.ident, &i.attrs) {
            self.has_colliding_symbols = true;
        }

        if self.inside_fn_body {
            self.skipped_stats.nested_function += 1;
            return;
        }

        if is_cfg_test(&i.attrs) {
            self.skipped_stats.cfg_test += 1;
            self.visit_skipped_fn_body(&i.block);
            return;
        }

        if i.sig.constness.is_some() {
            self.skipped_stats.const_fn += 1;
            self.visit_skipped_fn_body(&i.block);
            return;
        }

        if i.sig.abi.is_some() {
            self.skipped_stats.extern_abi += 1;
            self.visit_skipped_fn_body(&i.block);
            return;
        }

        if has_inline_attribute(&i.attrs) {
            self.skipped_stats.inline_attribute += 1;
            self.visit_skipped_fn_body(&i.block);
            return;
        }

        if has_instrument_attribute(&i.attrs) {
            self.skipped_stats.handwritten_otel += 1;
            self.visit_skipped_fn_body(&i.block);
            return;
        }

        if is_directly_self_recursive(&i.sig.ident, &i.block) {
            self.skipped_stats.self_recursive += 1;
            self.visit_skipped_fn_body(&i.block);
            return;
        }

        if body_has_handwritten_otel(&i.block, self.file_has_otel) {
            self.skipped_stats.handwritten_otel += 1;
            self.visit_skipped_fn_body(&i.block);
            return;
        }

        if let Some(imp) = &self.current_impl {
            if let Some(trait_ident) = &imp.trait_ident {
                if trait_ident == "Drop" && i.sig.ident == "drop" {
                    self.skipped_stats.drop_implementation += 1;
                    self.visit_skipped_fn_body(&i.block);
                    return;
                }
                if matches!(
                    trait_ident.as_str(),
                    "Deref" | "DerefMut" | "AsRef" | "AsMut" | "Borrow" | "BorrowMut"
                ) {
                    self.skipped_stats.adapter_trait += 1;
                    self.visit_skipped_fn_body(&i.block);
                    return;
                }
            }
        }

        let byte_range = i.span().byte_range();
        let body_byte_range = i.block.span().byte_range();
        let is_async = i.sig.asyncness.is_some();
        let is_generic = !i.sig.generics.params.is_empty();
        let returns_result = is_result_return_type(&i.sig.output);
        let returns_mut_reference = returns_mut_reference(&i.sig.output);
        let returns_reference_or_lifetime = returns_reference_or_lifetime(&i.sig.output);

        let (kind, function_name, has_enclosing_generics) = match &self.current_impl {
            Some(imp) => match &imp.trait_name {
                Some(tr) => (
                    FunctionKind::TraitMethod {
                        trait_name: tr.clone(),
                        type_name: imp.type_name.clone(),
                    },
                    normalize_type_str(&format!("<{} as {}>::{}", imp.type_name, tr, i.sig.ident)),
                    imp.is_generic,
                ),
                None => (
                    FunctionKind::InherentMethod {
                        type_name: imp.type_name.clone(),
                    },
                    normalize_type_str(&format!("{}::{}", imp.type_name, i.sig.ident)),
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
            returns_result,
            returns_mut_reference,
            returns_reference_or_lifetime,
        });

        let prev = self.inside_fn_body;
        self.inside_fn_body = true;
        syn::visit::visit_impl_item_fn(self, i);
        self.inside_fn_body = prev;
    }
}

/// Check if the function return type is syntactically a `Result` per §16.10.
fn is_result_return_type(output: &syn::ReturnType) -> bool {
    if let syn::ReturnType::Type(_, ty) = output {
        if let syn::Type::Path(type_path) = &**ty {
            if let Some(last_seg) = type_path.path.segments.last() {
                return last_seg.ident == "Result";
            }
        }
    }
    false
}

/// Check if the function return type contains a mutable reference (`&mut`).
///
/// Functions returning `&mut` (e.g. `Result<&mut T, E>` or `&mut T`) cannot be
/// wrapped in an immediately-invoked closure without tripping borrow-checker errors
/// (C1: captured variable cannot escape `FnMut` closure body). Such functions fall
/// back to prefix-only instrumentation.
pub(crate) fn returns_mut_reference(output: &syn::ReturnType) -> bool {
    struct MutRefVisitor {
        has_mut_ref: bool,
    }

    impl<'ast> syn::visit::Visit<'ast> for MutRefVisitor {
        fn visit_type_reference(&mut self, node: &'ast syn::TypeReference) {
            if node.mutability.is_some() {
                self.has_mut_ref = true;
                return;
            }
            syn::visit::visit_type_reference(self, node);
        }
    }

    if let syn::ReturnType::Type(_, ty) = output {
        let mut visitor = MutRefVisitor { has_mut_ref: false };
        syn::visit::visit_type(&mut visitor, ty);
        if visitor.has_mut_ref {
            return true;
        }
        let s = quote::quote!(#output).to_string();
        if s.contains("& mut") || s.contains("&mut") {
            return true;
        }
    }
    false
}

/// Check if the function return type contains any non-static reference (`&`) or explicit lifetime argument (`'a`, `'_`).
///
/// Functions returning references or carrying lifetimes (e.g. `&T`, `&mut T`, `Result<MutName<'_>, E>`)
/// fall back to prefix-only instrumentation to prevent `FnMut` closure escape borrow errors
/// on mutable accessors (including aliased `&mut`).
pub(crate) fn returns_reference_or_lifetime(output: &syn::ReturnType) -> bool {
    struct RefOrLifetimeVisitor {
        has_ref_or_lifetime: bool,
    }

    impl<'ast> syn::visit::Visit<'ast> for RefOrLifetimeVisitor {
        fn visit_type_reference(&mut self, node: &'ast syn::TypeReference) {
            // Any mutable reference triggers the fallback
            if node.mutability.is_some() {
                self.has_ref_or_lifetime = true;
                return;
            }
            // Non-static shared reference triggers fallback (&str, &'a str, etc.)
            if let Some(lt) = &node.lifetime {
                if lt.ident != "static" {
                    self.has_ref_or_lifetime = true;
                    return;
                }
            } else {
                // Anonymous/elided shared reference
                self.has_ref_or_lifetime = true;
                return;
            }
            syn::visit::visit_type_reference(self, node);
        }

        fn visit_lifetime(&mut self, node: &'ast syn::Lifetime) {
            if node.ident != "static" {
                self.has_ref_or_lifetime = true;
            }
        }
    }

    if let syn::ReturnType::Type(_, ty) = output {
        let mut visitor = RefOrLifetimeVisitor {
            has_ref_or_lifetime: false,
        };
        syn::visit::visit_type(&mut visitor, ty);
        if visitor.has_ref_or_lifetime {
            return true;
        }
        let s = quote::quote!(#output).to_string();
        if s.contains("& mut") || s.contains("&mut") || s.contains("'_") {
            return true;
        }
    }
    false
}

/// Span-creating explicit instrumentation attributes that define a new trace span.
/// Per S10, functions bearing these attributes must not be automatically instrumented.
const SPAN_CREATING_ATTRIBUTES: &[&str] = &["instrument", "instrument_span"];

/// Context-propagating explicit instrumentation attributes that propagate existing context
/// without creating a new span (e.g. upstream draft opentelemetry-rust-contrib#791).
/// In P2.2, these are conservatively skipped to prevent identifier shadowing collisions (__otel_cx).
const CONTEXT_PROPAGATING_ATTRIBUTES: &[&str] = &["propagate_context"];

/// Check for existing instrumentation attributes (both span-creating and context-propagating).
///
/// Matches the attribute path's final segment against known sets (e.g. `#[instrument]`,
/// `#[tracing::instrument]`, `#[tracing_attributes::instrument]`, `#[::tracing::instrument]`,
/// `#[otel_instrument::instrument]`, and `#[propagate_context]`).
fn has_instrument_attribute(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        let path = attr.path();
        if let Some(last_segment) = path.segments.last() {
            let ident = &last_segment.ident;
            if SPAN_CREATING_ATTRIBUTES.iter().any(|&s| ident == s)
                || CONTEXT_PROPAGATING_ATTRIBUTES.iter().any(|&s| ident == s)
            {
                return true;
            }
        }
    }
    false
}

/// Detect if the function is directly self-recursive (calls itself by name in its body).
///
/// NOTE on conservative recursion detection (R7):
/// For a *path* call we match if the path's last segment matches the function name (e.g. `foo()`,
/// `Self::foo()`, `crate::foo()`, `super::foo()`). As documented in R7, this intentionally accepts
/// a conservative false positive: calling `OtherType::helper()` inside a function named
/// `fn helper()` is classified as recursive and skipped. Distinguishing the two needs type
/// resolution we do not have, so the conservative side is taken deliberately.
///
/// For a *method* call the receiver is checked, which needs no type resolution: only `self.foo()`
/// counts. `self.inner.foo()`, `v.foo()` and `other.foo()` are different functions that merely
/// share a name, and skipping them was over-suppression rather than conservatism - it fired on
/// `fn clone()` calling `self.inner.clone()` and `fn len()` calling `self.lock_items().len()` in
/// `census-0.4.2`.
/// Whether `expr` is literally the receiver `self`, as opposed to `self.field` or any other
/// expression that merely ends in a call of the same name.
fn is_bare_self(expr: &syn::Expr) -> bool {
    matches!(expr, syn::Expr::Path(p) if p.qself.is_none() && p.path.is_ident("self"))
}

fn is_directly_self_recursive(fn_ident: &syn::Ident, block: &syn::Block) -> bool {
    struct RecursionDetector<'a> {
        target: &'a syn::Ident,
        found: bool,
    }

    impl<'ast> Visit<'ast> for RecursionDetector<'_> {
        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            if let syn::Expr::Path(expr_path) = &*call.func {
                if let Some(last_segment) = expr_path.path.segments.last() {
                    if last_segment.ident == *self.target {
                        self.found = true;
                        return;
                    }
                }
            }
            syn::visit::visit_expr_call(self, call);
        }

        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            // Only `self.foo()` is self-recursion. A receiver of any other shape - `self.inner`,
            // a local, a call result - names a different function that happens to share an ident.
            if call.method == *self.target && is_bare_self(&call.receiver) {
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

/// Check whether the receiver expression is literally an identifier named `tracer`.
fn is_receiver_tracer(expr: &syn::Expr) -> bool {
    if let syn::Expr::Path(expr_path) = expr {
        if expr_path.path.is_ident("tracer") {
            return true;
        }
    }
    false
}

/// Detect if the body already contains hand-written OpenTelemetry span creation or attachment.
///
/// Per R10: prevents double-instrumenting functions that manually start spans via
/// `tracer.start(...)`, wrap futures with `.with_context(...)`, or invoke `__otel_` hooks.
fn body_has_handwritten_otel(block: &syn::Block, file_has_otel: bool) -> bool {
    struct OtelDetector {
        file_has_otel: bool,
        found: bool,
    }

    impl<'ast> Visit<'ast> for OtelDetector {
        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            if call.method == "with_context" {
                // Check if argument is a closure (e.g. `anyhow::Context::with_context(self, || "msg")`).
                // OTel's FutureExt::with_context takes a Context value, NOT a closure:
                // `FutureExt::with_context(self, cx: Context)`.
                // Anyhow/Eyre takes: `with_context<C, F>(self, f: F) where F: FnOnce() -> C`.
                // Since OTel's signature structurally cannot accept a closure argument,
                // `arg is Expr::Closure => this is anyhow/eyre, not OTel`.
                let is_closure = call.args.len() == 1
                    && matches!(call.args.first(), Some(syn::Expr::Closure(_)));
                if !is_closure {
                    self.found = true;
                    return;
                }
            } else if call.method == "start" {
                // RESIDUAL TRADEOFF NOTE:
                // Unlike `with_context`, which is disambiguated call-by-call by closure argument shape,
                // `.start()` corroboration is file-scoped ("does the file import opentelemetry").
                // A file with one genuinely-instrumented async function (which imports opentelemetry)
                // and an unrelated `Timer::start()` call elsewhere in the same file will still misfire
                // on the timer call, because the corroborating check is file-scoped rather than call-scoped.
                // This significantly reduces false positives across the crate graph (since files calling
                // unrelated .start() rarely import opentelemetry), but is a documented residual tradeoff.
                if self.file_has_otel || is_receiver_tracer(&call.receiver) {
                    self.found = true;
                    return;
                }
            }
            syn::visit::visit_expr_method_call(self, call);
        }

        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            if let syn::Expr::Path(expr_path) = &*call.func {
                let s = quote::quote!(#expr_path).to_string();
                // Match exact ABI symbols or paths containing opentelemetry.
                // NOTE: We deliberately do NOT match bare "tracer" substring (e.g. ray_tracer::render()).
                if s.contains("__otel_") || s.contains("opentelemetry") {
                    self.found = true;
                    return;
                }
            }
            syn::visit::visit_expr_call(self, call);
        }
    }

    let mut detector = OtelDetector {
        file_has_otel,
        found: false,
    };
    detector.visit_block(block);
    detector.found
}
