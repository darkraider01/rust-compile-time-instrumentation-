#![feature(rustc_private)]

extern crate rustc_driver;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;

use std::env;
use std::path::{Path, PathBuf};

use instrument_semantics::{EligibilityPolicy, FunctionFacts, FunctionShape, P23_MARKER};
use rustc_errors::Applicability;
use rustc_hir::intravisit::{self, FnKind, Visitor};
use rustc_hir::{BodyId, FnDecl, ItemKind};
use rustc_middle::ty::TyCtxt;
use rustc_span::Span;

#[derive(Clone)]
struct DriverConfig {
    roots: Vec<PathBuf>,
    selected_packages: Vec<String>,
    crate_name: String,
    has_opentelemetry: bool,
}

struct P23Visitor<'tcx> {
    tcx: TyCtxt<'tcx>,
    config: DriverConfig,
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
        self.check_function(kind, body, span, def_id);
        intravisit::walk_fn(self, kind, decl, body_id, def_id);
    }
}

impl<'tcx> P23Visitor<'tcx> {
    fn check_function(
        &self,
        kind: FnKind<'tcx>,
        body: &'tcx rustc_hir::Body<'tcx>,
        span: Span,
        def_id: rustc_hir::def_id::LocalDefId,
    ) {
        let body_span = body.value.span;
        if span.from_expansion() || body_span.from_expansion() {
            return;
        }
        let shape = match kind {
            FnKind::ItemFn(..) => FunctionShape::FreeFunction,
            FnKind::Method(..) => {
                let hir_id = self.tcx.local_def_id_to_hir_id(def_id);
                let parent = self.tcx.hir_get_parent_item(hir_id).def_id;
                let ItemKind::Impl(implementation) = self.tcx.hir_expect_item(parent).kind else {
                    return;
                };
                if implementation.of_trait.is_some() {
                    return;
                }
                FunctionShape::InherentMethod
            }
            FnKind::Closure => return,
        };

        let source_map = self.tcx.sess.source_map();
        let Some(file_name) = source_map.span_to_filename(body_span).into_local_path() else {
            return;
        };
        let selected_package = env::var("CARGO_PKG_NAME").ok().is_some_and(|name| {
            self.config
                .selected_packages
                .iter()
                .any(|package| package == &name)
        });
        if !is_owned_source(&file_name, &self.config.roots) && !selected_package {
            return;
        }
        let Ok(original) = source_map.span_to_snippet(body_span) else {
            return;
        };
        let facts = FunctionFacts {
            function_name: self.tcx.item_name(def_id.to_def_id()).to_string(),
            crate_name: self.config.crate_name.clone(),
            shape,
            is_async: self.tcx.asyncness(def_id).is_async(),
            first_party: true,
            already_instrumented: original.contains(P23_MARKER),
            has_opentelemetry: self.config.has_opentelemetry,
        };
        let Ok(plan) = EligibilityPolicy::plan(&facts) else {
            return;
        };
        let replacement = instrument_body(
            &original,
            &plan.span.tracer_scope,
            &plan.span.span_name,
            plan.marker,
        );
        let mut diagnostic = self
            .tcx
            .dcx()
            .struct_span_warn(body_span, "P2.3 first-party instrumentation is available");
        diagnostic.span_suggestion(
            body_span,
            "insert an idempotent OpenTelemetry instrumentation block",
            replacement,
            Applicability::MachineApplicable,
        );
        diagnostic.emit();
    }
}

fn is_owned_source(source: &Path, roots: &[PathBuf]) -> bool {
    source
        .extension()
        .is_some_and(|extension| extension == "rs")
        && source.file_name().is_none_or(|name| name != "build.rs")
        && roots.iter().any(|root| source.starts_with(root))
}

fn instrument_body(original: &str, tracer_scope: &str, span_name: &str, marker: &str) -> String {
    let inner = original
        .trim()
        .strip_prefix('{')
        .and_then(|source| source.strip_suffix('}'))
        .unwrap_or(original)
        .trim();
    format!(
        "{{\n    {marker}\n    let __cargo_instrument_rust_tracer = opentelemetry::global::tracer({tracer_scope:?});\n    let __cargo_instrument_rust_span = opentelemetry::trace::Tracer::start(&__cargo_instrument_rust_tracer, {span_name:?});\n    let __cargo_instrument_rust_cx = <opentelemetry::Context as opentelemetry::trace::TraceContextExt>::current_with_span(__cargo_instrument_rust_span);\n    let _cargo_instrument_rust_guard = __cargo_instrument_rust_cx.attach();\n    {inner}\n}}"
    )
}

struct P23Callbacks {
    config: DriverConfig,
}

impl rustc_driver::Callbacks for P23Callbacks {
    fn after_analysis(
        &mut self,
        _compiler: &rustc_interface::interface::Compiler,
        tcx: TyCtxt<'_>,
    ) -> rustc_driver::Compilation {
        if !self.config.roots.is_empty() && self.config.has_opentelemetry {
            tcx.hir_visit_all_item_likes_in_crate(&mut P23Visitor {
                tcx,
                config: self.config.clone(),
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
    let config = DriverConfig {
        roots: env::var_os("CARGO_INSTRUMENT_RUST_ROOTS")
            .map(|roots| env::split_paths(&roots).collect())
            .unwrap_or_default(),
        selected_packages: env::var("CARGO_INSTRUMENT_RUST_PACKAGES")
            .map(|packages| packages.split(';').map(str::to_owned).collect())
            .unwrap_or_default(),
        crate_name: crate_name(&args).unwrap_or_else(|| "unknown".into()),
        has_opentelemetry: args.iter().any(|arg| arg.contains("opentelemetry")),
    };
    rustc_driver::run_compiler(&args, &mut P23Callbacks { config });
}

fn crate_name(args: &[String]) -> Option<String> {
    args.windows(2)
        .find(|window| window[0] == "--crate-name")
        .map(|window| window[1].clone())
}
