#![feature(rustc_private)]

extern crate rustc_ast;
extern crate rustc_driver;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_interface;
extern crate rustc_lint;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;

use std::env;
use std::path::{Path, PathBuf};

use instrument_semantics::{
    EligibilityPolicy, ExecutionLifecycle, FunctionFacts, FunctionShape, InstrumentationPlan,
    P23_MARKER,
};
use rustc_errors::Applicability;
use rustc_hir::intravisit::{self, FnKind, Visitor};
use rustc_hir::{BodyId, Constness, FnDecl, ItemKind};
use rustc_lint::Lint;
use rustc_middle::ty::TyCtxt;
use rustc_span::Span;
use serde::Deserialize;

// A non-tool lint name is intentionally used here. rustc validates tool-lint
// namespaces before a driver can register them; ordinary registered lint names
// are resolved after this driver's `register_lints` callback runs.
pub static INSTRUMENT: &Lint = &Lint {
    name: "cargo_instrument_instrument",
    default_level: rustc_lint::Warn,
    desc: "a first-party P2.3 instrumentation edit is available",
    edition_lint_opts: None,
    report_in_external_macro: false,
    future_incompatible: None,
    is_externally_loaded: false,
    feature_gate: None,
    crate_level_only: false,
    ignore_deny_warnings: true,
    ..Lint::default_fields_for_macro()
};

#[derive(Clone)]
struct DriverConfig {
    selected_packages: Vec<SelectedPackage>,
    crate_name: String,
    has_opentelemetry: bool,
}

#[derive(Clone, Deserialize)]
struct SelectedPackage {
    name: String,
    source_root: PathBuf,
}

struct P23Visitor<'tcx> {
    tcx: TyCtxt<'tcx>,
    config: DriverConfig,
    visited: std::collections::HashSet<rustc_hir::def_id::LocalDefId>,
}

impl<'tcx> Visitor<'tcx> for P23Visitor<'tcx> {
    type NestedFilter = rustc_middle::hir::nested_filter::All;

    fn maybe_tcx(&mut self) -> TyCtxt<'tcx> {
        self.tcx
    }

    fn visit_fn(
        &mut self,
        kind: FnKind<'tcx>,
        decl: &'tcx FnDecl<'tcx>,
        body_id: BodyId,
        span: Span,
        def_id: rustc_hir::def_id::LocalDefId,
    ) {
        let body = self.tcx.hir_body(body_id);
        self.check_function(kind, decl, body, span, def_id);
        intravisit::walk_fn(self, kind, decl, body_id, def_id);
    }
}

impl<'tcx> P23Visitor<'tcx> {
    fn check_function(
        &mut self,
        kind: FnKind<'tcx>,
        decl: &'tcx FnDecl<'tcx>,
        body: &'tcx rustc_hir::Body<'tcx>,
        span: Span,
        def_id: rustc_hir::def_id::LocalDefId,
    ) {
        if !self.visited.insert(def_id) {
            return;
        }
        let body_span = body.value.span;
        if span.from_expansion()
            || body_span.from_expansion()
            || matches!(kind.constness(), Constness::Const { .. })
            || !kind
                .header()
                .is_some_and(|header| header.abi.is_rustic_abi())
        {
            return;
        }
        let shape = if is_nested_function(self.tcx, def_id) {
            FunctionShape::NestedLocalFunction
        } else {
            match kind {
                FnKind::ItemFn(..) => FunctionShape::FreeFunction,
                FnKind::Method(..) => {
                    let hir_id = self.tcx.local_def_id_to_hir_id(def_id);
                    let parent = self.tcx.hir_get_parent_item(hir_id).def_id;
                    let ItemKind::Impl(implementation) = self.tcx.hir_expect_item(parent).kind
                    else {
                        return;
                    };
                    if implementation.of_trait.is_some() {
                        return;
                    }
                    FunctionShape::InherentMethod
                }
                FnKind::Closure => return,
            }
        };
        let source_map = self.tcx.sess.source_map();
        let Some(file_name) = source_map.span_to_filename(body_span).into_local_path() else {
            return;
        };
        let Some(package) =
            selected_package_for_current_compilation(&self.config.selected_packages)
        else {
            return;
        };
        if !is_owned_source(&file_name, package) {
            return;
        }
        let Ok(original) = source_map.span_to_snippet(body_span) else {
            return;
        };
        let hir_id = self.tcx.local_def_id_to_hir_id(def_id);
        let is_async = kind.asyncness().is_async();
        let facts = FunctionFacts {
            function_name: self.tcx.item_name(def_id.to_def_id()).to_string(),
            crate_name: self.config.crate_name.clone(),
            shape,
            is_async,
            returns_result: check_returns_result(self.tcx, def_id, decl, is_async),
            is_directly_recursive: check_is_directly_recursive(self.tcx, def_id, body),
            first_party: true,
            already_instrumented: original.contains(P23_MARKER),
            has_explicit_instrumentation: has_explicit_instrumentation(
                self.tcx, hir_id, body, &original,
            ),
            has_opentelemetry: self.config.has_opentelemetry,
        };
        let Ok(plan) = EligibilityPolicy::plan(&facts) else {
            return;
        };
        let hir_id = self.tcx.local_def_id_to_hir_id(def_id);
        self.tcx.emit_node_span_lint(
            INSTRUMENT,
            hir_id,
            body_span,
            rustc_errors::DiagDecorator(|diagnostic: &mut rustc_errors::Diag<'_, ()>| {
                diagnostic.primary_message("p2.3 first-party instrumentation is available");
                diagnostic.span_suggestion(
                    body_span,
                    "insert an idempotent OpenTelemetry instrumentation block",
                    instrument_body(&original, &plan),
                    Applicability::MachineApplicable,
                );
            }),
        );
    }
}

fn has_explicit_instrumentation<'tcx>(
    tcx: TyCtxt<'tcx>,
    hir_id: rustc_hir::HirId,
    body: &rustc_hir::Body<'tcx>,
    original: &str,
) -> bool {
    // 1. Direct item attributes (inert/unexpanded, e.g. #[instrument], #[instrument_span], #[propagate_context])
    let attrs = tcx.hir_attrs(hir_id);
    for attr in attrs {
        if let Some(last) = attr.path().last() {
            let s = last.as_str();
            if matches!(s, "instrument" | "instrument_span" | "propagate_context") {
                return true;
            }
        }
    }

    // 2. Procedural macro expansions inside the body (e.g. #[tracing::instrument])
    struct MacroFinder<'tcx> {
        tcx: TyCtxt<'tcx>,
        found: bool,
    }
    impl<'tcx> intravisit::Visitor<'tcx> for MacroFinder<'tcx> {
        type NestedFilter = rustc_middle::hir::nested_filter::All;
        fn maybe_tcx(&mut self) -> TyCtxt<'tcx> {
            self.tcx
        }
        fn visit_expr(&mut self, ex: &'tcx rustc_hir::Expr<'tcx>) {
            if self.found {
                return;
            }
            let data = ex.span.ctxt().outer_expn_data();
            if let rustc_span::hygiene::ExpnKind::Macro(rustc_span::hygiene::MacroKind::Attr, sym) =
                data.kind
            {
                let s = sym.as_str();
                if s.ends_with("instrument")
                    || s.ends_with("propagate_context")
                    || s.ends_with("instrument_span")
                {
                    self.found = true;
                    return;
                }
            }
            intravisit::walk_expr(self, ex);
        }
        fn visit_stmt(&mut self, stmt: &'tcx rustc_hir::Stmt<'tcx>) {
            if self.found {
                return;
            }
            let data = stmt.span.ctxt().outer_expn_data();
            if let rustc_span::hygiene::ExpnKind::Macro(rustc_span::hygiene::MacroKind::Attr, sym) =
                data.kind
            {
                let s = sym.as_str();
                if s.ends_with("instrument")
                    || s.ends_with("propagate_context")
                    || s.ends_with("instrument_span")
                {
                    self.found = true;
                    return;
                }
            }
            intravisit::walk_stmt(self, stmt);
        }
    }
    let mut finder = MacroFinder { tcx, found: false };
    intravisit::walk_body(&mut finder, body);
    if finder.found {
        return true;
    }

    // 3. Hand-written OpenTelemetry span creation or attachment in body (R10 / ADR-009 parity)
    if original.contains("__otel_")
        || original.contains("tracer.start")
        || original.contains("FutureExt::with_context")
    {
        return true;
    }

    false
}

fn selected_package_for_current_compilation(
    selected_packages: &[SelectedPackage],
) -> Option<&SelectedPackage> {
    let name = env::var("CARGO_PKG_NAME").ok()?;
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR")?);
    selected_packages.iter().find(|package| {
        package.name == name
            && same_file::is_same_file(&manifest_dir, &package.source_root).unwrap_or(false)
    })
}

fn is_owned_source(source: &Path, package: &SelectedPackage) -> bool {
    let Ok(canonical_source) = source.canonicalize() else {
        return false;
    };
    let Ok(canonical_root) = package.source_root.canonicalize() else {
        return false;
    };
    canonical_source
        .extension()
        .is_some_and(|extension| extension == "rs")
        && source.file_name().is_none_or(|name| name != "build.rs")
        && canonical_source.starts_with(canonical_root)
}

fn is_nested_function(tcx: TyCtxt<'_>, def_id: rustc_hir::def_id::LocalDefId) -> bool {
    let hir_id = tcx.local_def_id_to_hir_id(def_id);
    let mut current = hir_id;
    loop {
        let parent = tcx.hir_get_parent_item(current);
        if parent.def_id == rustc_span::def_id::CRATE_DEF_ID
            || parent.def_id == current.owner.def_id
        {
            return false;
        }
        match tcx.def_kind(parent.def_id) {
            rustc_hir::def::DefKind::Fn
            | rustc_hir::def::DefKind::AssocFn
            | rustc_hir::def::DefKind::Closure => return true,
            _ => {
                current = tcx.local_def_id_to_hir_id(parent.def_id);
            }
        }
    }
}

/// Semantic Result detection: resolves the declared return type through rustc type information.
/// For an ADT, identifies standard `core::result::Result` using rustc diagnostic-item identity (`sym::Result`),
/// naturally handling aliases (`std::io::Result<T>`) while avoiding false positives from custom types named `Result`.
/// Sync functions returning non-static or mutable references fall back to prefix-only instrumentation (§16.10 / C1),
/// whereas async Result functions remain eligible because they use the Future lifecycle rather than a closure.
fn check_returns_result<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: rustc_hir::def_id::LocalDefId,
    decl: &'tcx rustc_hir::FnDecl<'tcx>,
    is_async: bool,
) -> bool {
    let rustc_hir::FnRetTy::Return(mut hir_ty) = decl.output else {
        return false;
    };

    // For async fn, the declared return type in decl.output is an OpaqueDef (impl Future<Output = T>).
    // Extract the inner Output type from the future bounds.
    if let rustc_hir::TyKind::OpaqueDef(ref opaque_ty) = hir_ty.kind {
        for bound in opaque_ty.bounds {
            if let rustc_hir::GenericBound::Trait(ref poly_trait_ref) = bound {
                for segment in poly_trait_ref.trait_ref.path.segments {
                    if let Some(args) = segment.args {
                        for constraint in args.constraints {
                            if constraint.ident.as_str() == "Output" {
                                if let rustc_hir::AssocItemConstraintKind::Equality {
                                    term: rustc_hir::Term::Ty(inner_ty),
                                } = constraint.kind
                                {
                                    hir_ty = inner_ty;
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let typeck = tcx.typeck(def_id);
    let is_result = if let rustc_hir::TyKind::Path(ref qpath) = hir_ty.kind {
        let res = match qpath {
            rustc_hir::QPath::Resolved(_, path) => path.res,
            rustc_hir::QPath::TypeRelative(..) => typeck.qpath_res(qpath, hir_ty.hir_id),
        };
        match res {
            rustc_hir::def::Res::Def(rustc_hir::def::DefKind::Enum, did) => {
                tcx.is_diagnostic_item(rustc_span::symbol::sym::Result, did)
            }
            rustc_hir::def::Res::Def(rustc_hir::def::DefKind::TyAlias, alias_did) => {
                let aliased = tcx.type_of(alias_did).skip_binder();
                is_diagnostic_result(tcx, aliased)
            }
            _ => false,
        }
    } else {
        false
    };

    if !is_result {
        return false;
    }

    if !is_async {
        let fn_sig = tcx
            .fn_sig(def_id.to_def_id())
            .instantiate_identity()
            .skip_binder();
        let output_ty = fn_sig.output();
        if sync_closure_ineligible_for_references(output_ty) {
            return false;
        }
    }

    true
}

fn is_diagnostic_result<'tcx>(tcx: TyCtxt<'tcx>, ty: rustc_middle::ty::Ty<'tcx>) -> bool {
    match ty.kind() {
        rustc_middle::ty::TyKind::Adt(def, _) => {
            tcx.is_diagnostic_item(rustc_span::symbol::sym::Result, def.did())
        }
        _ => false,
    }
}

fn sync_closure_ineligible_for_references<'tcx>(ty: rustc_middle::ty::Ty<'tcx>) -> bool {
    for arg in ty.walk() {
        if let Some(t) = arg.as_type() {
            if let rustc_middle::ty::TyKind::Ref(region, _, mutability) = t.kind() {
                if mutability.is_mut() || !region.is_static() {
                    return true;
                }
            }
        }
        if let Some(region) = arg.as_region() {
            if !region.is_static() {
                return true;
            }
        }
    }
    false
}

/// Direct self-recursion exclusion parity (§12.3): direct self-recursive functions are excluded
/// to prevent unbounded recursive span explosion. In HIR, exact DefId comparison is used for both
/// path calls and type-dependent method calls, improving precision over syn AST name-based matching
/// without false positives, while retaining the same exclusion policy.
fn check_is_directly_recursive<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: rustc_hir::def_id::LocalDefId,
    body: &'tcx rustc_hir::Body<'tcx>,
) -> bool {
    let typeck = tcx.typeck(def_id);
    let target = def_id.to_def_id();

    struct RecursionFinder<'a, 'tcx> {
        tcx: TyCtxt<'tcx>,
        typeck: &'a rustc_middle::ty::TypeckResults<'tcx>,
        target: rustc_span::def_id::DefId,
        found: bool,
    }

    impl<'a, 'tcx> intravisit::Visitor<'tcx> for RecursionFinder<'a, 'tcx> {
        type NestedFilter = rustc_middle::hir::nested_filter::OnlyBodies;

        fn maybe_tcx(&mut self) -> TyCtxt<'tcx> {
            self.tcx
        }

        fn visit_expr(&mut self, expr: &'tcx rustc_hir::Expr<'tcx>) {
            if self.found {
                return;
            }
            match expr.kind {
                rustc_hir::ExprKind::Call(func, _) => {
                    if let rustc_hir::ExprKind::Path(ref qpath) = func.kind {
                        let res = self.typeck.qpath_res(qpath, func.hir_id);
                        if let rustc_hir::def::Res::Def(_, called_did) = res {
                            if called_did == self.target {
                                self.found = true;
                                return;
                            }
                        }
                    }
                }
                rustc_hir::ExprKind::MethodCall(..) => {
                    if let Some(called_did) = self.typeck.type_dependent_def_id(expr.hir_id) {
                        if called_did == self.target {
                            self.found = true;
                            return;
                        }
                    }
                }
                _ => {}
            }
            intravisit::walk_expr(self, expr);
        }
    }

    let mut finder = RecursionFinder {
        tcx,
        typeck,
        target,
        found: false,
    };
    intravisit::walk_body(&mut finder, body);
    finder.found
}

fn instrument_body(original: &str, plan: &InstrumentationPlan) -> String {
    let inner = original
        .trim()
        .strip_prefix('{')
        .and_then(|source| source.strip_suffix('}'))
        .unwrap_or(original)
        .trim();
    let prelude = format!(
        "    {}\n    let __cargo_instrument_rust_tracer = opentelemetry::global::tracer({:?});\n    let __cargo_instrument_rust_span = opentelemetry::trace::Tracer::start(&__cargo_instrument_rust_tracer, {:?});\n    let __cargo_instrument_rust_cx = <opentelemetry::Context as opentelemetry::trace::TraceContextExt>::current_with_span(__cargo_instrument_rust_span);",
        plan.marker, plan.span.tracer_scope, plan.span.span_name
    );
    match plan.lifecycle {
        ExecutionLifecycle::SyncScopedContext => {
            if plan.returns_result {
                format!(
                    "{{\n{prelude}\n    let _cargo_instrument_rust_guard = __cargo_instrument_rust_cx.clone().attach();\n    #[allow(clippy::redundant_closure_call)]\n    let __cargo_instrument_rust_res: Result<_, _> = (|| {{\n        {inner}\n    }})();\n    if __cargo_instrument_rust_res.is_err() {{\n        opentelemetry::trace::TraceContextExt::span(&__cargo_instrument_rust_cx)\n            .set_status(opentelemetry::trace::Status::error(\"\"));\n    }}\n    __cargo_instrument_rust_res\n}}"
                )
            } else {
                format!(
                    "{{\n{prelude}\n    let _cargo_instrument_rust_guard = __cargo_instrument_rust_cx.attach();\n    {inner}\n}}"
                )
            }
        }
        ExecutionLifecycle::AsyncFutureContext => {
            if plan.returns_result {
                format!(
                    "{{\n{prelude}\n    let __cargo_instrument_rust_res: Result<_, _> = opentelemetry::trace::FutureExt::with_context(async move {{\n        {inner}\n    }}, __cargo_instrument_rust_cx.clone()).await;\n    if __cargo_instrument_rust_res.is_err() {{\n        opentelemetry::trace::TraceContextExt::span(&__cargo_instrument_rust_cx)\n            .set_status(opentelemetry::trace::Status::error(\"\"));\n    }}\n    __cargo_instrument_rust_res\n}}"
                )
            } else {
                format!(
                    "{{\n{prelude}\n    opentelemetry::trace::FutureExt::with_context(async move {{\n        {inner}\n    }}, __cargo_instrument_rust_cx).await\n}}"
                )
            }
        }
    }
}

struct P23Callbacks {
    config: DriverConfig,
}

impl rustc_driver::Callbacks for P23Callbacks {
    fn config(&mut self, config: &mut rustc_interface::Config) {
        let previous = config.register_lints.take();
        config.register_lints = Some(Box::new(move |session, lint_store| {
            if let Some(previous) = &previous {
                previous(session, lint_store);
            }
            lint_store.register_lints(&[&INSTRUMENT]);
        }));
    }

    fn after_analysis(
        &mut self,
        _compiler: &rustc_interface::interface::Compiler,
        tcx: TyCtxt<'_>,
    ) -> rustc_driver::Compilation {
        if !self.config.selected_packages.is_empty() && self.config.has_opentelemetry {
            tcx.hir_visit_all_item_likes_in_crate(&mut P23Visitor {
                tcx,
                config: self.config.clone(),
                visited: std::collections::HashSet::new(),
            });
        }
        rustc_driver::Compilation::Continue
    }
}

fn main() {
    let mut args: Vec<String> = env::args().collect();
    if args.get(1).is_some_and(|arg| {
        Path::new(arg)
            .file_stem()
            .is_some_and(|stem| stem == "rustc")
    }) {
        args.remove(1);
    }
    if !args.iter().any(|arg| arg == "--sysroot") {
        if let Ok(sysroot) = env::var("CARGO_INSTRUMENT_RUST_SYSROOT") {
            args.push("--sysroot".into());
            args.push(sysroot);
        }
    }
    normalize_lint_controls(&mut args);
    // These are driver arguments, not Cargo rustflags. Ordinary diagnostics
    // are capped while this registered lint is force-warned for rustfix.
    args.push("--cap-lints=allow".into());
    args.push("--force-warn=cargo_instrument_instrument".into());
    let config = DriverConfig {
        selected_packages: env::var("CARGO_INSTRUMENT_RUST_SELECTED_PACKAGES")
            .ok()
            .and_then(|packages| serde_json::from_str(&packages).ok())
            .unwrap_or_default(),
        crate_name: crate_name(&args).unwrap_or_else(|| "unknown".into()),
        has_opentelemetry: has_opentelemetry_extern(&args),
    };
    rustc_driver::run_compiler(&args, &mut P23Callbacks { config });
}

fn normalize_lint_controls(args: &mut Vec<String>) {
    let mut filtered = Vec::with_capacity(args.len() + 2);
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--cap-lints" || args[index] == "--force-warn" {
            index += if index + 1 < args.len() { 2 } else { 1 };
        } else if args[index].starts_with("--cap-lints=")
            || args[index].starts_with("--force-warn=")
        {
            index += 1;
        } else {
            filtered.push(args[index].clone());
            index += 1;
        }
    }
    *args = filtered;
}

fn has_opentelemetry_extern(args: &[String]) -> bool {
    let mut index = 0;
    while index < args.len() {
        let spec = if args[index] == "--extern" {
            index += 1;
            args.get(index).map(String::as_str)
        } else {
            args[index].strip_prefix("--extern=")
        };
        if let Some(spec) = spec {
            let extern_name = spec.split('=').next().unwrap_or(spec);
            let extern_name = extern_name.rsplit(':').next().unwrap_or(extern_name);
            if extern_name == "opentelemetry" {
                return true;
            }
        }
        index += 1;
    }
    false
}

fn crate_name(args: &[String]) -> Option<String> {
    args.windows(2)
        .find(|window| window[0] == "--crate-name")
        .map(|window| window[1].clone())
}

#[cfg(test)]
mod tests {
    use super::{has_opentelemetry_extern, normalize_lint_controls};

    fn args(arguments: &[&str]) -> Vec<String> {
        arguments.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn detects_only_the_exact_opentelemetry_extern_name() {
        assert!(has_opentelemetry_extern(&args(&[
            "--extern",
            "opentelemetry=C:/registry/libopentelemetry.rlib"
        ])));
        assert!(!has_opentelemetry_extern(&args(&[
            "--extern",
            "opentelemetry_sdk=C:/registry/libopentelemetry_sdk.rlib"
        ])));
        assert!(!has_opentelemetry_extern(&args(&[
            "C:/contains-opentelemetry-but-is-not-an-extern"
        ])));
    }

    #[test]
    fn removes_existing_lint_caps_and_force_warns_while_preserving_build_flags() {
        let mut arguments = args(&[
            "rustc",
            "--cap-lints",
            "warn",
            "--cap-lints=deny",
            "--force-warn",
            "unused-parens",
            "--force-warn=dead-code",
            "--cfg",
            "p23_fixture_cfg",
            "-C",
            "opt-level=2",
            "fixture.rs",
        ]);
        normalize_lint_controls(&mut arguments);
        assert_eq!(
            arguments,
            args(&[
                "rustc",
                "--cfg",
                "p23_fixture_cfg",
                "-C",
                "opt-level=2",
                "fixture.rs"
            ])
        );
    }
}
