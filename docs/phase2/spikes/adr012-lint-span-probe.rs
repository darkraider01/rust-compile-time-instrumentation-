// HISTORICAL SPIKE — NOT PRODUCTION TOOLING.
//
// ADR-012 feasibility evidence for HIR body-span reachability. This standalone
// probe is not built by the workspace and must not be used as the P2.3 driver.
// Production architecture and the Cargo-fix integration contract are frozen in
// ADR-013 (`docs/phase2/decision-records.md`).
//
// Minimal rustc driver that inspects every fn body and reports whether its span
// is usable for a source-level suggestion.
#![feature(rustc_private)]

extern crate rustc_driver;
extern crate rustc_hir;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;

use rustc_hir::intravisit::{self, Visitor};
use rustc_hir::{BodyId, FnDecl};
use rustc_middle::ty::TyCtxt;
use rustc_span::Span;

struct Probe<'tcx> {
    tcx: TyCtxt<'tcx>,
    seen: std::collections::HashSet<String>,
}

impl<'tcx> Visitor<'tcx> for Probe<'tcx> {
    type NestedFilter = rustc_middle::hir::nested_filter::All;

    fn maybe_tcx(&mut self) -> TyCtxt<'tcx> {
        self.tcx
    }

    fn visit_fn(
        &mut self,
        fk: intravisit::FnKind<'tcx>,
        fd: &'tcx FnDecl<'tcx>,
        body_id: BodyId,
        span: Span,
        def_id: rustc_span::def_id::LocalDefId,
    ) {
        let name = match fk {
            intravisit::FnKind::ItemFn(ident, ..) => ident.to_string(),
            intravisit::FnKind::Method(ident, ..) => ident.to_string(),
            intravisit::FnKind::Closure => "<closure>".to_string(),
        };

        let body = self.tcx.hir_body(body_id);
        let body_span = body.value.span;
        let sm = self.tcx.sess.source_map();

        let snippet = sm.span_to_snippet(body_span);

        if name == "<closure>" || span.from_expansion() || body_span.from_expansion() {
            let _ = (fd, def_id);
            intravisit::walk_fn(self, fk, fd, body_id, def_id);
            return;
        }
        if !self.seen.insert(name.clone()) {
            let _ = (fd, def_id);
            intravisit::walk_fn(self, fk, fd, body_id, def_id);
            return;
        }
        if let Ok(orig) = &snippet {
            let inner = orig.trim().trim_start_matches('{').trim_end_matches('}').trim();
            println!("=== {name} ===");
            println!("  body span usable : true");
            println!("  original         : {:?}", orig.replace('\n', " "));
            println!("  suggestion text  :");
            println!("      {{");
            println!("          let __otel_tracer = opentelemetry::global::tracer(\"fx\");");
            println!("          let __otel_span = opentelemetry::trace::Tracer::start(&__otel_tracer, \"{name}\");");
            println!("          let __otel_cx = opentelemetry::Context::current_with_span(__otel_span);");
            println!("          let _guard = __otel_cx.attach();");
            println!("          {inner}");
            println!("      }}");
        }
        let _ = (fd, def_id);
        intravisit::walk_fn(self, fk, fd, body_id, def_id);
    }
}

struct Callbacks;

impl rustc_driver::Callbacks for Callbacks {
    fn after_analysis(&mut self, _c: &rustc_interface::interface::Compiler, tcx: TyCtxt<'_>) -> rustc_driver::Compilation {
        println!("---- lintspike: fn body span report ----");
        let mut probe = Probe { tcx, seen: Default::default() };
        tcx.hir_visit_all_item_likes_in_crate(&mut probe);
        println!("---- end report ----");
        rustc_driver::Compilation::Continue
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    rustc_driver::run_compiler(&args, &mut Callbacks);
}
