use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cargo_instrument::ast::analyze_source_str;
use cargo_instrument::candidate::UnsafePolicy;
use cargo_instrument::transform::{
    transform_source_str_with_native_otel_and_spawns, Emitter, NativeOtelEmitter, SentinelEmitter,
    TrampolineEmitter, TransformationPlan,
};
use opentelemetry::trace::{TraceContextExt as _, Tracer as _};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

/// Compile-time generic bound asserting that the future type implements Send.
fn assert_is_send<T: Send>(val: T) -> T {
    val
}

// ---------------------------------------------------------------------------
// 1. Empirical proof of current uninstrumented loss vs call-site capture
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_uninstrumented_tokio_spawn_loses_parent_context() {
    let tracer = opentelemetry::global::tracer("test_tokio_spawn");
    let parent_span = tracer.start("enclosing_parent");
    let parent_cx = opentelemetry::Context::current_with_span(parent_span);
    let parent_span_id = parent_cx.span().span_context().span_id();
    let parent_trace_id = parent_cx.span().span_context().trace_id();

    let _guard = parent_cx.attach();

    // Plain, uninstrumented tokio::spawn loses caller TLS context at first poll
    let handle_unwrapped = tokio::spawn(async {
        opentelemetry::Context::current()
            .span()
            .span_context()
            .span_id()
    });
    let unwrapped_span_id = handle_unwrapped.await.unwrap();
    assert_eq!(
        unwrapped_span_id,
        opentelemetry::trace::SpanId::INVALID,
        "uninstrumented tokio::spawn must lose caller TLS context"
    );

    // Call-site context capture wrapped with FutureExt::with_context preserves context
    let handle_wrapped = tokio::spawn(assert_is_send(
        opentelemetry::trace::FutureExt::with_context(
            async {
                let active = opentelemetry::Context::current();
                (
                    active.span().span_context().trace_id(),
                    active.span().span_context().span_id(),
                )
            },
            opentelemetry::Context::current(),
        ),
    ));
    let (wrapped_trace_id, wrapped_span_id) = handle_wrapped.await.unwrap();
    assert_eq!(wrapped_trace_id, parent_trace_id);
    assert_eq!(wrapped_span_id, parent_span_id);
}

// ---------------------------------------------------------------------------
// 2. Conservative AST path recognition (Issue #6 refinement 1)
// ---------------------------------------------------------------------------

#[test]
fn test_ast_conservative_tokio_spawn_recognition() {
    let source = r#"
fn test_calls() {
    // Unambiguous forms (MUST match)
    tokio::spawn(async { 1 });
    tokio::task::spawn(async { 2 });
    ::tokio::spawn(async { 3 });
    ::tokio::task::spawn(async { 4 });

    // Ambiguous or unrelated forms (MUST NOT match)
    spawn(async { 5 });
    task::spawn(async { 6 });
    other::spawn(async { 7 });
    std::thread::spawn(|| { 8 });
    tokio::task::spawn_blocking(|| { 9 });
}
"#;

    let report =
        analyze_source_str("test_crate", Path::new("src/lib.rs"), source).expect("analyze source");

    // Exactly 4 unambiguous tokio spawn calls must be discovered
    assert_eq!(
        report.spawn_sites.len(),
        4,
        "only unambiguous tokio spawn paths must be recognized"
    );

    let mut matched_snippets: Vec<&str> = report
        .spawn_sites
        .iter()
        .map(|s| &source[s.arg_byte_range.clone()])
        .collect();
    matched_snippets.sort();

    assert_eq!(
        matched_snippets,
        vec!["async { 1 }", "async { 2 }", "async { 3 }", "async { 4 }"]
    );
}

// ---------------------------------------------------------------------------
// 3. Structural idempotence vs substring false positives (Issue #6 refinement 4)
// ---------------------------------------------------------------------------

#[test]
fn test_ast_structural_idempotence() {
    let source = r#"
fn test_idempotence() {
    let cx = opentelemetry::Context::current();

    // Already wrapped at top level via FutureExt::with_context (MUST NOT double-wrap)
    tokio::spawn(opentelemetry::trace::FutureExt::with_context(async { 1 }, cx.clone()));

    // Already wrapped at top level via method call (MUST NOT double-wrap)
    tokio::spawn(async { 2 }.with_context(cx));

    // Inner with_context statement inside async block (MUST still wrap the spawn boundary)
    tokio::spawn(async {
        let inner_fut = async { 3 };
        let _ = inner_fut.with_context(opentelemetry::Context::current()).await;
        4
    });
}
"#;

    let report =
        analyze_source_str("test_crate", Path::new("src/lib.rs"), source).expect("analyze source");

    // Only the 3rd spawn call must be discovered; the first two are structurally already wrapped
    assert_eq!(
        report.spawn_sites.len(),
        1,
        "top-level with_context must be skipped, but inner with_context must NOT prevent spawn wrapping"
    );
    let snippet = &source[report.spawn_sites[0].arg_byte_range.clone()];
    assert!(snippet.starts_with("async {"));
    assert!(snippet.contains("inner_fut.with_context"));
}

// ---------------------------------------------------------------------------
// 4. Emitter scoping (Issue #6 refinement 5)
// ---------------------------------------------------------------------------

#[test]
fn test_emitter_tokio_spawn_scoping() {
    let native = NativeOtelEmitter::new("my_crate");
    assert!(
        native.handles_tokio_spawn(),
        "NativeOtelEmitter must handle tokio spawn"
    );

    let sentinel = SentinelEmitter;
    assert!(
        !sentinel.handles_tokio_spawn(),
        "SentinelEmitter must not handle tokio spawn"
    );

    let trampoline =
        TrampolineEmitter::new("my_crate", Some("2021".to_string()), UnsafePolicy::Allowed);
    assert!(
        !trampoline.handles_tokio_spawn(),
        "TrampolineEmitter must not handle tokio spawn (Tier-2 C-ABI untouched)"
    );
}

// ---------------------------------------------------------------------------
// 5. Transformation composition and spawn-only files (Issue #6 refinements 2 & 3)
// ---------------------------------------------------------------------------

#[test]
fn test_transform_composition_with_candidate_and_spawns() {
    let source = r#"
pub async fn parent_job() {
    tokio::spawn(async {
        dependency_work().await;
    });
}
"#;

    let report =
        analyze_source_str("my_crate", Path::new("src/lib.rs"), source).expect("analyze source");
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.spawn_sites.len(), 1);

    let plan = TransformationPlan::build_with_emitter_and_spawns(
        source,
        &report.candidates,
        &report.spawn_sites,
        &NativeOtelEmitter::new("my_crate"),
    )
    .expect("build plan");

    let transformed = plan.apply(source).expect("apply plan");

    // Must have function anchor and span prefix
    assert!(transformed.contains("/* __cargo_instrument_anchor: \"parent_job\" */"));
    assert!(transformed.contains("opentelemetry::global::tracer(\"my_crate\")"));

    // Must wrap the spawn argument
    assert!(transformed.contains(
        "tokio::spawn(opentelemetry::trace::FutureExt::with_context(async {\n        dependency_work().await;\n    }, opentelemetry::Context::current()));"
    ));
}

#[test]
fn test_transform_nested_tokio_spawns() {
    let source = r#"
fn run() {
    tokio::spawn(async {
        tokio::spawn(async {
            deep_work().await;
        }).await.unwrap();
    });
}
"#;

    let report =
        analyze_source_str("my_crate", Path::new("src/lib.rs"), source).expect("analyze source");
    assert_eq!(
        report.spawn_sites.len(),
        2,
        "must discover both outer and inner spawn"
    );

    let plan = TransformationPlan::build_with_emitter_and_spawns(
        source,
        &report.candidates,
        &report.spawn_sites,
        &NativeOtelEmitter::new("my_crate"),
    )
    .expect("build plan");

    let transformed = plan.apply(source).expect("apply plan");

    // Both spawns must be wrapped cleanly
    assert!(
        transformed.contains("tokio::spawn(opentelemetry::trace::FutureExt::with_context(async {")
    );
    assert!(transformed.contains(
        "deep_work().await;\n        }, opentelemetry::Context::current())).await.unwrap();"
    ));
}

#[test]
fn test_transform_spawn_only_file_with_zero_candidates() {
    // A file where all functions are excluded/skipped (e.g. inline test functions or non-candidate items)
    let source = r#"
#[inline]
fn launch_worker() {
    tokio::spawn(async {
        do_work().await;
    });
}
"#;

    let report =
        analyze_source_str("my_crate", Path::new("src/lib.rs"), source).expect("analyze source");
    assert_eq!(
        report.candidates.len(),
        0,
        "inline function must be skipped from function instrumentation"
    );
    assert_eq!(
        report.spawn_sites.len(),
        1,
        "spawn site must still be discovered"
    );

    let transformed = transform_source_str_with_native_otel_and_spawns(
        source,
        "my_crate",
        &report.candidates,
        &report.spawn_sites,
    )
    .expect("transform source");
    assert!(
        transformed.contains("tokio::spawn(opentelemetry::trace::FutureExt::with_context(async {")
    );
}

// ---------------------------------------------------------------------------
// 6. Multi-thread Tokio runtime execution with nested spawns and dependency spans
// ---------------------------------------------------------------------------

// Simulated dependency function instrumented with native OpenTelemetry
async fn dependency_work() -> u32 {
    let dep_tracer = opentelemetry::global::tracer("dep_crate");
    let dep_span = opentelemetry::trace::Tracer::span_builder(&dep_tracer, "dependency_work")
        .with_kind(opentelemetry::trace::SpanKind::Internal)
        .start(&dep_tracer);
    let dep_cx =
        <opentelemetry::Context as opentelemetry::trace::TraceContextExt>::current_with_span(
            dep_span,
        );
    opentelemetry::trace::FutureExt::with_context(
        async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            42
        },
        dep_cx,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_runtime_multi_thread_context_propagation() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    opentelemetry::global::set_tracer_provider(provider);

    let tracer = opentelemetry::global::tracer("test_app");
    let parent = tracer.start("app_parent");
    let parent_cx = opentelemetry::Context::current_with_span(parent);

    let thread_count = Arc::new(AtomicUsize::new(0));
    let thread_count_clone = thread_count.clone();

    // Execute under active parent context
    let cx_for_outer = parent_cx.clone();
    let outer_guard = cx_for_outer.attach();

    // Outer spawn: uses exact transformed shape emitted by cargo-instrument
    let outer_handle = tokio::spawn(assert_is_send(
        opentelemetry::trace::FutureExt::with_context(
            async move {
                thread_count_clone.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(5)).await;

                // Inner spawn inside outer task: exact transformed shape
                let inner_handle = tokio::spawn(assert_is_send(
                    opentelemetry::trace::FutureExt::with_context(
                        async move { dependency_work().await },
                        opentelemetry::Context::current(),
                    ),
                ));
                inner_handle.await.unwrap()
            },
            opentelemetry::Context::current(),
        ),
    ));

    let res = outer_handle.await.unwrap();
    assert_eq!(res, 42);

    drop(outer_guard);
    parent_cx.span().end();

    // Check exported spans
    let spans = exporter.get_finished_spans().expect("get spans");
    let parent_span = spans
        .iter()
        .find(|s| s.name == "app_parent")
        .expect("parent span");
    let dep_span = spans
        .iter()
        .find(|s| s.name == "dependency_work")
        .expect("dep span");

    assert_eq!(
        dep_span.span_context.trace_id(),
        parent_span.span_context.trace_id(),
        "spawned dependency span must inherit enclosing parent TraceId"
    );
    assert_eq!(
        dep_span.parent_span_id,
        parent_span.span_context.span_id(),
        "spawned dependency span must have parent_span_id equal to enclosing parent SpanId"
    );

    // Assert exactly 2 spans (parent + dep), no synthetic task spans
    assert_eq!(
        spans.len(),
        2,
        "only app_parent and dependency_work must be exported (no synthetic task spans)"
    );

    // Assert no active context leak after completion
    assert_eq!(
        opentelemetry::Context::current()
            .span()
            .span_context()
            .span_id(),
        opentelemetry::trace::SpanId::INVALID,
        "no active context leak on caller thread"
    );
}
