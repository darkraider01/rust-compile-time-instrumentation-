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
use opentelemetry::trace::{
    SpanContext, SpanId, TraceContextExt as _, TraceFlags, TraceId, TraceState, Tracer as _,
};
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
    let parent_trace_id = TraceId::from(0x1111_2222_3333_4444_5555_6666_7777_8888_u128);
    let parent_span_id = SpanId::from(0x1111_2222_3333_4444_u64);
    let parent_cx = opentelemetry::Context::new().with_remote_span_context(SpanContext::new(
        parent_trace_id,
        parent_span_id,
        TraceFlags::SAMPLED,
        true,
        TraceState::default(),
    ));

    assert_ne!(parent_trace_id, TraceId::INVALID);
    assert_ne!(parent_span_id, SpanId::INVALID);

    let _guard = parent_cx.attach();
    let active_parent = opentelemetry::Context::current();
    assert_eq!(
        active_parent.span().span_context().trace_id(),
        parent_trace_id
    );
    assert_eq!(
        active_parent.span().span_context().span_id(),
        parent_span_id
    );

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

#[test]
fn test_ast_tokio_shadowing_excludes_syntactic_spawn_matches() {
    for source in [
        r#"
            mod tokio { pub fn spawn<T>(value: T) { let _ = value; } }
            fn run() { tokio::spawn(123); }
        "#,
        r#"
            use some_other_crate as tokio;
            fn run() { tokio::spawn(async {}); }
        "#,
        r#"
            extern crate something_else as tokio;
            fn run() { tokio::task::spawn(async {}); }
        "#,
        r#"
            struct tokio;
            fn run() { tokio::spawn(async {}); }
        "#,
        r#"
            enum tokio { Runtime }
            fn run() { tokio::spawn(async {}); }
        "#,
        r#"
            union tokio { raw: usize }
            fn run() { tokio::spawn(async {}); }
        "#,
        r#"
            trait tokio {}
            fn run() { tokio::spawn(async {}); }
        "#,
        r#"
            type tokio = usize;
            fn run() { tokio::spawn(async {}); }
        "#,
        r#"
            fn run<tokio: Runtime>() { tokio::spawn(async {}); }
        "#,
    ] {
        let report =
            analyze_source_str("shadowed_tokio", Path::new("src/lib.rs"), source).expect("parse");
        assert!(
            report.spawn_sites.is_empty(),
            "locally shadowed tokio must never be rewritten: {source}"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. Structural idempotence vs substring false positives (Issue #6 refinement 4)
// ---------------------------------------------------------------------------

#[test]
fn test_ast_structural_idempotence() {
    let source = r#"
fn test_idempotence() {
    let cx = opentelemetry::Context::current();

    // The exact project-generated wrapper MUST NOT double-wrap.
    tokio::spawn(opentelemetry::trace::FutureExt::with_context(async { 1 }, cx.clone()));

    // Arbitrary method calls cannot be attributed to OpenTelemetry without name resolution.
    tokio::spawn(async { 2 }.with_context(cx));

    // An unrelated path ending in with_context is not an OpenTelemetry wrapper.
    tokio::spawn(custom_lib::with_context(async { 3 }, cx));

    // Inner with_context statement inside async block (MUST still wrap the spawn boundary)
    tokio::spawn(async {
        let inner_fut = async { 4 };
        let _ = inner_fut.with_context(opentelemetry::Context::current()).await;
        4
    });
}
"#;

    let report =
        analyze_source_str("test_crate", Path::new("src/lib.rs"), source).expect("analyze source");

    // Only the exact OpenTelemetry wrapper is skipped; method and custom-path calls remain eligible.
    assert_eq!(
        report.spawn_sites.len(),
        3,
        "only the explicit OpenTelemetry wrapper may suppress spawn instrumentation"
    );
    let snippets: Vec<_> = report
        .spawn_sites
        .iter()
        .map(|site| &source[site.arg_byte_range.clone()])
        .collect();
    assert!(snippets
        .iter()
        .any(|snippet| snippet.contains(".with_context(cx)")));
    assert!(snippets
        .iter()
        .any(|snippet| snippet.contains("custom_lib::with_context")));
    assert!(snippets
        .iter()
        .any(|snippet| snippet.contains("inner_fut.with_context")));
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

// ---------------------------------------------------------------------------
// 7. H3: Cargo-authoritative Tokio package identity verification
// ---------------------------------------------------------------------------

#[test]
fn test_h3_metadata_identity_proof_all_adversarial_cases() {
    use cargo_instrument::session::{CargoDepEdge, SessionPlan};
    use std::collections::HashMap;

    let temp = tempfile::tempdir().expect("tempdir");
    let real_app_dir = temp.path().join("real_app");
    let fake_app_dir = temp.path().join("fake_app");
    let renamed_app_dir = temp.path().join("renamed_app");
    let unrelated_app_dir = temp.path().join("unrelated_app");
    let multi_v02_dir = temp.path().join("multi_v02");
    std::fs::create_dir_all(real_app_dir.join("src")).unwrap();
    std::fs::create_dir_all(fake_app_dir.join("src")).unwrap();
    std::fs::create_dir_all(renamed_app_dir.join("src")).unwrap();
    std::fs::create_dir_all(unrelated_app_dir.join("src")).unwrap();
    std::fs::create_dir_all(multi_v02_dir.join("src")).unwrap();

    let real_src = real_app_dir.join("src/lib.rs");
    let fake_src = fake_app_dir.join("src/lib.rs");
    let renamed_src = renamed_app_dir.join("src/lib.rs");
    let unrelated_src = unrelated_app_dir.join("src/lib.rs");
    let v02_src = multi_v02_dir.join("src/lib.rs");

    let real_id = "real-app 0.1.0 (path+file:///real_app)";
    let fake_app_id = "fake-app 0.1.0 (path+file:///fake_app)";
    let renamed_app_id = "renamed-app 0.1.0 (path+file:///renamed_app)";
    let unrelated_app_id = "unrelated-app 0.1.0 (path+file:///unrelated_app)";
    let v02_id = "multi-v02 0.1.0 (path+file:///multi_v02)";

    let tokio_v1_id = "registry+https://example.invalid#index#tokio@1.43.0";
    let tokio_v02_id = "registry+https://example.invalid#index#tokio@0.2.25";
    let fake_runtime_id = "path+file:///fake-runtime#0.1.0";

    let mut package_manifest_dirs = HashMap::new();
    package_manifest_dirs.insert(real_id.to_string(), real_app_dir);
    package_manifest_dirs.insert(fake_app_id.to_string(), fake_app_dir);
    package_manifest_dirs.insert(renamed_app_id.to_string(), renamed_app_dir);
    package_manifest_dirs.insert(unrelated_app_id.to_string(), unrelated_app_dir);
    package_manifest_dirs.insert(v02_id.to_string(), multi_v02_dir);

    let mut package_names_by_id = HashMap::new();
    package_names_by_id.insert(real_id.to_string(), "real-app".to_string());
    package_names_by_id.insert(fake_app_id.to_string(), "fake-app".to_string());
    package_names_by_id.insert(renamed_app_id.to_string(), "renamed-app".to_string());
    package_names_by_id.insert(unrelated_app_id.to_string(), "unrelated-app".to_string());
    package_names_by_id.insert(v02_id.to_string(), "multi-v02".to_string());
    package_names_by_id.insert(tokio_v1_id.to_string(), "tokio".to_string());
    package_names_by_id.insert(tokio_v02_id.to_string(), "tokio".to_string());
    package_names_by_id.insert(fake_runtime_id.to_string(), "fake-runtime".to_string());

    let mut package_dependencies = HashMap::new();

    // 1. Real app depends on tokio@1.43.0 with binding name "tokio"
    package_dependencies.insert(
        real_id.to_string(),
        vec![CargoDepEdge {
            binding_name: "tokio".to_string(),
            package_id: tokio_v1_id.to_string(),
            kinds: vec![None],
        }],
    );

    // 2. Fake app has tokio = { package = "fake-runtime", ... } (binding name is "tokio", but package is fake-runtime)
    package_dependencies.insert(
        fake_app_id.to_string(),
        vec![CargoDepEdge {
            binding_name: "tokio".to_string(),
            package_id: fake_runtime_id.to_string(),
            kinds: vec![None],
        }],
    );

    // 3. Renamed app has my_tokio = { package = "tokio", ... } (binding name is "my_tokio")
    package_dependencies.insert(
        renamed_app_id.to_string(),
        vec![CargoDepEdge {
            binding_name: "my_tokio".to_string(),
            package_id: tokio_v1_id.to_string(),
            kinds: vec![None],
        }],
    );

    // 4. Unrelated app has no tokio dependency
    package_dependencies.insert(unrelated_app_id.to_string(), vec![]);

    // 5. Multi-version app depends on tokio@0.2.25 with binding name "tokio"
    package_dependencies.insert(
        v02_id.to_string(),
        vec![CargoDepEdge {
            binding_name: "tokio".to_string(),
            package_id: tokio_v02_id.to_string(),
            kinds: vec![None],
        }],
    );

    let plan = SessionPlan {
        package_manifest_dirs,
        package_names_by_id,
        package_dependencies,
        ..Default::default()
    };

    let tokio_args = vec![
        "--extern".to_string(),
        "tokio=target/libtokio.rlib".to_string(),
    ];
    let my_tokio_args = vec![
        "--extern".to_string(),
        "my_tokio=target/libtokio.rlib".to_string(),
    ];

    // Case 1: Real Tokio -> accepted
    assert!(
        plan.unit_has_real_tokio_binding(&real_src, &tokio_args),
        "real Tokio binding must be proved and accepted"
    );

    // Case 2: Fake package renamed to tokio -> rejected
    assert!(
        !plan.unit_has_real_tokio_binding(&fake_src, &tokio_args),
        "fake runtime renamed to tokio must be rejected"
    );

    // Case 3: Actual Tokio renamed away to my_tokio -> rejected (syntax and binding require 'tokio')
    assert!(
        !plan.unit_has_real_tokio_binding(&renamed_src, &my_tokio_args),
        "renamed tokio binding (my_tokio) must not be accepted for tokio::spawn propagation"
    );
    // Also verify conservative AST recognition ignores my_tokio::spawn
    let renamed_ast = analyze_source_str(
        "renamed_app",
        &renamed_src,
        "fn run() { my_tokio::spawn(async { 1 }); }",
    )
    .expect("parse source");
    assert!(
        renamed_ast.spawn_sites.is_empty(),
        "syntactic recognizer must only match tokio::spawn, not my_tokio::spawn"
    );

    // Case 4: Tokio elsewhere in graph -> rejected for unit without dependency
    assert!(
        !plan.unit_has_real_tokio_binding(&unrelated_src, &tokio_args),
        "tokio elsewhere in graph must not enable unit lacking tokio dependency"
    );

    // Case 5: Multiple Tokio versions -> resolves per edge (v0.2 accepted for multi_v02)
    assert!(
        plan.unit_has_real_tokio_binding(&v02_src, &tokio_args),
        "independent tokio version edge must resolve per package edge"
    );

    // Case 6: Unknown/stale unit -> rejected
    let unknown_src = temp.path().join("unknown/src/lib.rs");
    assert!(
        !plan.unit_has_real_tokio_binding(&unknown_src, &tokio_args),
        "unknown unit must be rejected safely"
    );
}

#[test]
fn test_h3_adversarial_fake_runtime_renamed_to_tokio_live_cargo_build() {
    use std::fs;
    use std::process::Command;

    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let workspace_root = temp_dir.path();

    // 1. Create fake-runtime crate
    let fake_runtime_dir = workspace_root.join("fake_runtime");
    fs::create_dir_all(fake_runtime_dir.join("src")).expect("create fake_runtime/src");
    fs::write(
        fake_runtime_dir.join("Cargo.toml"),
        r#"[package]
name = "fake-runtime"
version = "0.1.0"
edition = "2021"
"#,
    )
    .expect("write fake_runtime Cargo.toml");

    fs::write(
        fake_runtime_dir.join("src/lib.rs"),
        r#"pub fn spawn<F>(f: F)
where
    F: Send + 'static,
{
    let _ = f;
}
"#,
    )
    .expect("write fake_runtime src/lib.rs");

    // 2. Create app crate depending on fake-runtime renamed to tokio
    let app_dir = workspace_root.join("app");
    fs::create_dir_all(app_dir.join("src")).expect("create app/src");
    fs::write(
        app_dir.join("Cargo.toml"),
        r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { package = "fake-runtime", path = "../fake_runtime" }
"#,
    )
    .expect("write app Cargo.toml");

    fs::write(
        app_dir.join("src/main.rs"),
        r#"fn work() -> i32 {
    100
}

fn main() {
    let w = work();
    assert_eq!(w, 100);
    tokio::spawn(async {
        42
    });
}
"#,
    )
    .expect("write app src/main.rs");

    let cargo_instrument_bin = env!("CARGO_BIN_EXE_cargo-instrument");
    let target_dir = workspace_root.join("target").join("instrumented");

    // 3. Build with cargo-instrument as RUSTC_WRAPPER
    let output = Command::new("cargo")
        .arg("build")
        .arg("--target-dir")
        .arg(&target_dir)
        .current_dir(&app_dir)
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

    // 4. Assert that H3 identity check suppressed tokio::spawn rewriting
    assert!(
        stderr.contains("suppressed 1 tokio::spawn site(s): unit lacks Cargo-authoritative Tokio package binding"),
        "Wrapper should log suppression of tokio::spawn site for fake runtime. stderr:\n{stderr}"
    );

    // 5. Inspect mirrored main.rs: functions transformed, but spawn NOT rewritten
    let instrumented_sources = target_dir
        .join("debug")
        .join("deps")
        .join("instrumented_sources");

    let app_pkg_dir = fs::read_dir(&instrumented_sources)
        .ok()
        .and_then(|entries| {
            entries.flatten().map(|e| e.path()).find(|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                name == "app" || name.starts_with("app-")
            })
        });

    if let Some(mirrored_dir) = app_pkg_dir {
        let mirrored_main =
            fs::read_to_string(mirrored_dir.join("src/main.rs")).expect("read mirrored main.rs");
        // Function candidate must be anchored
        assert!(
            mirrored_main.contains("/* __cargo_instrument_anchor: \"work\" */"),
            "mirrored main.rs should instrument function candidate 'work'"
        );
        // Spawn must NOT be rewritten with FutureExt::with_context
        assert!(
            !mirrored_main.contains("FutureExt::with_context"),
            "fake runtime tokio::spawn MUST NOT be rewritten with FutureExt::with_context"
        );
        assert!(
            !mirrored_main.contains("opentelemetry::Context::current()"),
            "fake runtime tokio::spawn MUST NOT inject OpenTelemetry Context::current()"
        );
        assert!(
            mirrored_main.contains("tokio::spawn(async {"),
            "original tokio::spawn call must remain intact"
        );
    }

    // 6. Run the compiled binary to ensure it executes cleanly
    let bin_name = if cfg!(windows) { "app.exe" } else { "app" };
    let app_bin = target_dir.join("debug").join(bin_name);
    let run_output = Command::new(&app_bin)
        .output()
        .expect("execute compiled app binary");
    assert!(
        run_output.status.success(),
        "compiled app binary failed to run! stderr: {}",
        String::from_utf8_lossy(&run_output.stderr)
    );
}

#[test]
fn test_h3_valid_tokio_artifact_selected() {
    use cargo_instrument::session::{CargoDepEdge, R4Profile, SessionPlan, TokioArtifact};
    use std::collections::HashMap;

    let temp = tempfile::tempdir().expect("tempdir");
    let app_dir = temp.path().join("app");
    std::fs::create_dir_all(app_dir.join("src")).unwrap();
    let src = app_dir.join("src/lib.rs");
    std::fs::write(&src, "pub fn run() {}\n").unwrap();

    let rlib = temp.path().join("libtokio-valid.rlib");
    std::fs::write(&rlib, b"valid tokio rlib").unwrap();

    let app_id = "app 0.1.0 (path+file:///app)";
    let tokio_id = "registry+https://example.invalid#index#tokio@1.43.0";

    let mut package_manifest_dirs = HashMap::new();
    package_manifest_dirs.insert(app_id.to_string(), app_dir.clone());

    let mut package_names_by_id = HashMap::new();
    package_names_by_id.insert(app_id.to_string(), "app".to_string());
    package_names_by_id.insert(tokio_id.to_string(), "tokio".to_string());

    let mut package_dependencies = HashMap::new();
    package_dependencies.insert(
        app_id.to_string(),
        vec![CargoDepEdge {
            binding_name: "tokio".to_string(),
            package_id: tokio_id.to_string(),
            kinds: vec![None],
        }],
    );

    let mut tokio_package_by_dependency = HashMap::new();
    tokio_package_by_dependency.insert(app_id.to_string(), tokio_id.to_string());

    let mut tokio_required_features_by_dependency = HashMap::new();
    tokio_required_features_by_dependency.insert(app_id.to_string(), vec!["rt".to_string()]);

    let tokio_artifact = TokioArtifact {
        package_id: tokio_id.to_string(),
        package_version: "1.43.0".to_string(),
        resolved_features: vec!["macros".to_string(), "rt".to_string(), "sync".to_string()],
        target: None,
        profile: R4Profile::default(),
        rlib_path: rlib.clone(),
    };

    let plan = SessionPlan {
        package_manifest_dirs,
        package_names_by_id,
        package_dependencies,
        tokio_package_by_dependency,
        tokio_required_features_by_dependency,
        tokio_artifacts: vec![tokio_artifact],
        ..Default::default()
    };

    let tokio_args = vec!["--extern".to_string(), format!("tokio={}", rlib.display())];

    assert!(
        plan.unit_has_real_tokio_metadata_binding(&src, &tokio_args),
        "metadata identity must pass"
    );

    let resolved = plan
        .tokio_artifact_for(&src, &tokio_args)
        .expect("tokio_artifact_for should succeed");
    assert!(resolved.is_some(), "exact Tokio artifact must be resolved");
    assert_eq!(resolved.unwrap().rlib_path, rlib);

    assert!(
        plan.unit_has_real_tokio_binding(&src, &tokio_args),
        "end-to-end unit_has_real_tokio_binding must succeed with valid artifact and 'rt' feature"
    );
    assert!(plan.validate_tokio_binding(&src, &tokio_args).is_ok());
}

#[test]
fn test_h3_adversarial_tokio_artifact_lacks_rt_feature() {
    use cargo_instrument::session::{CargoDepEdge, R4Profile, SessionPlan, TokioArtifact};
    use std::collections::HashMap;

    let temp = tempfile::tempdir().expect("tempdir");
    let app_dir = temp.path().join("app");
    std::fs::create_dir_all(app_dir.join("src")).unwrap();
    let src = app_dir.join("src/lib.rs");
    std::fs::write(&src, "pub fn run() {}\n").unwrap();

    let rlib = temp.path().join("libtokio-nort.rlib");
    std::fs::write(&rlib, b"tokio lacking rt").unwrap();

    let app_id = "app 0.1.0 (path+file:///app)";
    let tokio_id = "registry+https://example.invalid#index#tokio@1.43.0";

    let mut package_manifest_dirs = HashMap::new();
    package_manifest_dirs.insert(app_id.to_string(), app_dir.clone());

    let mut package_names_by_id = HashMap::new();
    package_names_by_id.insert(app_id.to_string(), "app".to_string());
    package_names_by_id.insert(tokio_id.to_string(), "tokio".to_string());

    let mut package_dependencies = HashMap::new();
    package_dependencies.insert(
        app_id.to_string(),
        vec![CargoDepEdge {
            binding_name: "tokio".to_string(),
            package_id: tokio_id.to_string(),
            kinds: vec![None],
        }],
    );

    let mut tokio_package_by_dependency = HashMap::new();
    tokio_package_by_dependency.insert(app_id.to_string(), tokio_id.to_string());

    let mut tokio_required_features_by_dependency = HashMap::new();
    tokio_required_features_by_dependency.insert(app_id.to_string(), vec!["rt".to_string()]);

    // Artifact only has "sync" and "macros", NOT "rt"
    let tokio_artifact = TokioArtifact {
        package_id: tokio_id.to_string(),
        package_version: "1.43.0".to_string(),
        resolved_features: vec!["macros".to_string(), "sync".to_string()],
        target: None,
        profile: R4Profile::default(),
        rlib_path: rlib.clone(),
    };

    let plan = SessionPlan {
        package_manifest_dirs,
        package_names_by_id,
        package_dependencies,
        tokio_package_by_dependency,
        tokio_required_features_by_dependency,
        tokio_artifacts: vec![tokio_artifact],
        ..Default::default()
    };

    let tokio_args = vec!["--extern".to_string(), format!("tokio={}", rlib.display())];

    // Metadata identity still passes
    assert!(plan.unit_has_real_tokio_metadata_binding(&src, &tokio_args));

    // Artifact resolution fails due to missing "rt" feature
    let err = plan
        .tokio_artifact_for(&src, &tokio_args)
        .expect_err("must fail when artifact lacks 'rt'");
    assert!(
        err.contains("lacks required feature(s): [\"rt\"]"),
        "error must specifically mention missing 'rt' feature; got: {err}"
    );

    // End-to-end check returns false to suppress spawn sites
    assert!(
        !plan.unit_has_real_tokio_binding(&src, &tokio_args),
        "unit_has_real_tokio_binding must return false when artifact lacks 'rt'"
    );
    assert!(plan.validate_tokio_binding(&src, &tokio_args).is_err());
}

#[test]
fn test_h3_tokio_artifact_rt_multi_thread_satisfies_rt_requirement() {
    use cargo_instrument::session::{CargoDepEdge, R4Profile, SessionPlan, TokioArtifact};
    use std::collections::HashMap;

    let temp = tempfile::tempdir().expect("tempdir");
    let app_dir = temp.path().join("app");
    std::fs::create_dir_all(app_dir.join("src")).unwrap();
    let src = app_dir.join("src/lib.rs");
    std::fs::write(&src, "pub fn run() {}\n").unwrap();

    let rlib = temp.path().join("libtokio-rtmulti.rlib");
    std::fs::write(&rlib, b"tokio rt-multi-thread").unwrap();

    let app_id = "app 0.1.0 (path+file:///app)";
    let tokio_id = "registry+https://example.invalid#index#tokio@1.43.0";

    let mut package_manifest_dirs = HashMap::new();
    package_manifest_dirs.insert(app_id.to_string(), app_dir.clone());

    let mut package_names_by_id = HashMap::new();
    package_names_by_id.insert(app_id.to_string(), "app".to_string());
    package_names_by_id.insert(tokio_id.to_string(), "tokio".to_string());

    let mut package_dependencies = HashMap::new();
    package_dependencies.insert(
        app_id.to_string(),
        vec![CargoDepEdge {
            binding_name: "tokio".to_string(),
            package_id: tokio_id.to_string(),
            kinds: vec![None],
        }],
    );

    let mut tokio_package_by_dependency = HashMap::new();
    tokio_package_by_dependency.insert(app_id.to_string(), tokio_id.to_string());

    // In Tokio, rt-multi-thread enables rt; compiler-artifact includes both
    let tokio_artifact = TokioArtifact {
        package_id: tokio_id.to_string(),
        package_version: "1.43.0".to_string(),
        resolved_features: vec![
            "macros".to_string(),
            "rt".to_string(),
            "rt-multi-thread".to_string(),
        ],
        target: None,
        profile: R4Profile::default(),
        rlib_path: rlib.clone(),
    };

    let plan = SessionPlan {
        package_manifest_dirs,
        package_names_by_id,
        package_dependencies,
        tokio_package_by_dependency,
        tokio_artifacts: vec![tokio_artifact],
        ..Default::default()
    };

    let tokio_args = vec!["--extern".to_string(), format!("tokio={}", rlib.display())];

    assert!(
        plan.unit_has_real_tokio_binding(&src, &tokio_args),
        "artifact with both 'rt' and 'rt-multi-thread' must satisfy requirement"
    );
}

#[test]
fn test_h3_tokio_artifact_target_mismatch_fails() {
    use cargo_instrument::session::{CargoDepEdge, R4Profile, SessionPlan, TokioArtifact};
    use std::collections::HashMap;

    let temp = tempfile::tempdir().expect("tempdir");
    let app_dir = temp.path().join("app");
    std::fs::create_dir_all(app_dir.join("src")).unwrap();
    let src = app_dir.join("src/lib.rs");
    std::fs::write(&src, "pub fn run() {}\n").unwrap();

    let rlib = temp.path().join("libtokio-linux.rlib");
    std::fs::write(&rlib, b"linux tokio").unwrap();

    let app_id = "app 0.1.0 (path+file:///app)";
    let tokio_id = "registry+https://example.invalid#index#tokio@1.43.0";

    let mut package_manifest_dirs = HashMap::new();
    package_manifest_dirs.insert(app_id.to_string(), app_dir.clone());

    let mut package_names_by_id = HashMap::new();
    package_names_by_id.insert(app_id.to_string(), "app".to_string());
    package_names_by_id.insert(tokio_id.to_string(), "tokio".to_string());

    let mut package_dependencies = HashMap::new();
    package_dependencies.insert(
        app_id.to_string(),
        vec![CargoDepEdge {
            binding_name: "tokio".to_string(),
            package_id: tokio_id.to_string(),
            kinds: vec![None],
        }],
    );

    let mut tokio_package_by_dependency = HashMap::new();
    tokio_package_by_dependency.insert(app_id.to_string(), tokio_id.to_string());

    // Artifact compiled for linux
    let tokio_artifact = TokioArtifact {
        package_id: tokio_id.to_string(),
        package_version: "1.43.0".to_string(),
        resolved_features: vec!["rt".to_string()],
        target: Some("x86_64-unknown-linux-gnu".to_string()),
        profile: R4Profile::default(),
        rlib_path: rlib.clone(),
    };

    let plan = SessionPlan {
        package_manifest_dirs,
        package_names_by_id,
        package_dependencies,
        tokio_package_by_dependency,
        tokio_artifacts: vec![tokio_artifact],
        ..Default::default()
    };

    let default_tokio_args = vec!["--extern".to_string(), format!("tokio={}", rlib.display())];

    // Invocations with mismatched target (e.g. host default None or wasm32) must fail
    let err = plan
        .tokio_artifact_for(&src, &default_tokio_args)
        .unwrap_err();
    assert!(err.contains("no Cargo-authoritative Tokio artifact"));
    assert!(!plan.unit_has_real_tokio_binding(&src, &default_tokio_args));

    let wasm_args = vec![
        "--target".to_string(),
        "wasm32-wasip1".to_string(),
        "--extern".to_string(),
        format!("tokio={}", rlib.display()),
    ];
    let err2 = plan.tokio_artifact_for(&src, &wasm_args).unwrap_err();
    assert!(err2.contains("no Cargo-authoritative Tokio artifact"));
    assert!(!plan.unit_has_real_tokio_binding(&src, &wasm_args));

    // Matching target succeeds
    let linux_args = vec![
        "--target".to_string(),
        "x86_64-unknown-linux-gnu".to_string(),
        "--extern".to_string(),
        format!("tokio={}", rlib.display()),
    ];
    assert!(plan.unit_has_real_tokio_binding(&src, &linux_args));
}

#[test]
fn test_h3_tokio_artifact_profile_mismatch_fails() {
    use cargo_instrument::session::{CargoDepEdge, R4Profile, SessionPlan, TokioArtifact};
    use std::collections::HashMap;

    let temp = tempfile::tempdir().expect("tempdir");
    let app_dir = temp.path().join("app");
    std::fs::create_dir_all(app_dir.join("src")).unwrap();
    let src = app_dir.join("src/lib.rs");
    std::fs::write(&src, "pub fn run() {}\n").unwrap();

    let rlib = temp.path().join("libtokio-dev.rlib");
    std::fs::write(&rlib, b"dev tokio").unwrap();

    let app_id = "app 0.1.0 (path+file:///app)";
    let tokio_id = "registry+https://example.invalid#index#tokio@1.43.0";

    let mut package_manifest_dirs = HashMap::new();
    package_manifest_dirs.insert(app_id.to_string(), app_dir.clone());

    let mut package_names_by_id = HashMap::new();
    package_names_by_id.insert(app_id.to_string(), "app".to_string());
    package_names_by_id.insert(tokio_id.to_string(), "tokio".to_string());

    let mut package_dependencies = HashMap::new();
    package_dependencies.insert(
        app_id.to_string(),
        vec![CargoDepEdge {
            binding_name: "tokio".to_string(),
            package_id: tokio_id.to_string(),
            kinds: vec![None],
        }],
    );

    let mut tokio_package_by_dependency = HashMap::new();
    tokio_package_by_dependency.insert(app_id.to_string(), tokio_id.to_string());

    let tokio_artifact = TokioArtifact {
        package_id: tokio_id.to_string(),
        package_version: "1.43.0".to_string(),
        resolved_features: vec!["rt".to_string()],
        target: None,
        profile: R4Profile {
            opt_level: "0".to_string(),
            debug_assertions: true,
            overflow_checks: true,
            test: false,
        },
        rlib_path: rlib.clone(),
    };

    let plan = SessionPlan {
        package_manifest_dirs,
        package_names_by_id,
        package_dependencies,
        tokio_package_by_dependency,
        tokio_artifacts: vec![tokio_artifact],
        ..Default::default()
    };

    // Release profile invocation (opt-level=3) encounters profile mismatch
    let release_args = vec![
        "-C".to_string(),
        "opt-level=3".to_string(),
        "--extern".to_string(),
        format!("tokio={}", rlib.display()),
    ];

    let err = plan.tokio_artifact_for(&src, &release_args).unwrap_err();
    assert!(err.contains("no Cargo-authoritative Tokio artifact"));
    assert!(!plan.unit_has_real_tokio_binding(&src, &release_args));

    // Matching profile succeeds
    let dev_args = vec!["--extern".to_string(), format!("tokio={}", rlib.display())];
    assert!(plan.unit_has_real_tokio_binding(&src, &dev_args));
}

#[test]
fn test_h3_tokio_extern_artifact_path_identity_enforcement() {
    use cargo_instrument::session::{CargoDepEdge, R4Profile, SessionPlan, TokioArtifact};
    use std::collections::HashMap;

    let temp = tempfile::tempdir().expect("tempdir");
    let app_dir = temp.path().join("app");
    std::fs::create_dir_all(app_dir.join("src")).unwrap();
    let src = app_dir.join("src/lib.rs");
    std::fs::write(&src, "pub fn run() {}\n").unwrap();

    let target_dir = temp.path().join("target");
    std::fs::create_dir_all(&target_dir).unwrap();
    let genuine_rlib = target_dir.join("libtokio.rlib");
    std::fs::write(&genuine_rlib, b"genuine tokio rlib").unwrap();

    let decoy_dir = temp.path().join("decoy");
    std::fs::create_dir_all(&decoy_dir).unwrap();
    let decoy_rlib = decoy_dir.join("libtokio.rlib");
    std::fs::write(&decoy_rlib, b"decoy tokio rlib").unwrap();

    let app_id = "app 0.1.0 (path+file:///app)";
    let tokio_id = "registry+https://example.invalid#index#tokio@1.43.0";

    let mut package_manifest_dirs = HashMap::new();
    package_manifest_dirs.insert(app_id.to_string(), app_dir.clone());

    let mut package_names_by_id = HashMap::new();
    package_names_by_id.insert(app_id.to_string(), "app".to_string());
    package_names_by_id.insert(tokio_id.to_string(), "tokio".to_string());

    let mut package_dependencies = HashMap::new();
    package_dependencies.insert(
        app_id.to_string(),
        vec![CargoDepEdge {
            binding_name: "tokio".to_string(),
            package_id: tokio_id.to_string(),
            kinds: vec![None],
        }],
    );

    let mut tokio_package_by_dependency = HashMap::new();
    tokio_package_by_dependency.insert(app_id.to_string(), tokio_id.to_string());

    let tokio_artifact = TokioArtifact {
        package_id: tokio_id.to_string(),
        package_version: "1.43.0".to_string(),
        resolved_features: vec!["rt".to_string()],
        target: None,
        profile: R4Profile::default(),
        rlib_path: genuine_rlib.clone(),
    };

    let plan = SessionPlan {
        package_manifest_dirs,
        package_names_by_id,
        package_dependencies,
        tokio_package_by_dependency,
        tokio_artifacts: vec![tokio_artifact],
        ..Default::default()
    };

    // 1. Exact match succeeds
    let exact_args = vec![
        "--extern".to_string(),
        format!("tokio={}", genuine_rlib.display()),
    ];
    assert!(plan.unit_has_real_tokio_binding(&src, &exact_args));

    // 2. Decoy path with same filename in different dir fails
    let decoy_args = vec![
        "--extern".to_string(),
        format!("tokio={}", decoy_rlib.display()),
    ];
    let err = plan.tokio_artifact_for(&src, &decoy_args).unwrap_err();
    assert!(
        err.contains("does not match active rustc --extern"),
        "mismatched path must be rejected; got: {err}"
    );
    assert!(!plan.unit_has_real_tokio_binding(&src, &decoy_args));

    // 3. Stale/non-existent path fails
    let stale_args = vec![
        "--extern".to_string(),
        format!(
            "tokio={}",
            temp.path().join("does_not_exist.rlib").display()
        ),
    ];
    let err2 = plan.tokio_artifact_for(&src, &stale_args).unwrap_err();
    assert!(err2.contains("does not match active rustc --extern"));
    assert!(!plan.unit_has_real_tokio_binding(&src, &stale_args));
}

#[test]
fn test_h3_missing_tokio_artifact_features_fails() {
    use cargo_instrument::SessionPlan;

    let temp = tempfile::tempdir().expect("tempdir");
    let rlib = temp.path().join("libtokio.rlib");
    std::fs::write(&rlib, b"tokio rlib").unwrap();

    let tokio_id = "registry+https://example.invalid#index#tokio@1.43.0";
    let metadata = serde_json::json!({
        "packages": [
            {"id": tokio_id, "name": "tokio", "version": "1.43.0"}
        ]
    });

    let message_without_features = serde_json::json!({
        "reason": "compiler-artifact",
        "package_id": tokio_id,
        "filenames": [rlib],
        "profile": {"opt_level": "0", "debug_assertions": true, "overflow_checks": true, "test": false}
    });

    let mut plan = SessionPlan::default();
    plan.add_tokio_artifacts_from_cargo_json(
        &metadata,
        format!("{message_without_features}\n").as_bytes(),
        None,
    )
    .expect("ingestion completes");

    assert!(
        plan.tokio_artifacts.is_empty(),
        "compiler-artifact lacking 'features' field must NOT be registered"
    );
}

#[test]
fn test_h3_multiple_ambiguous_tokio_artifacts_rejected() {
    use cargo_instrument::SessionPlan;

    let temp = tempfile::tempdir().expect("tempdir");
    let rlib1 = temp.path().join("libtokio1.rlib");
    let rlib2 = temp.path().join("libtokio2.rlib");
    std::fs::write(&rlib1, b"tokio rlib 1").unwrap();
    std::fs::write(&rlib2, b"tokio rlib 2").unwrap();

    let tokio_id = "registry+https://example.invalid#index#tokio@1.43.0";
    let metadata = serde_json::json!({
        "packages": [
            {"id": tokio_id, "name": "tokio", "version": "1.43.0"}
        ]
    });

    let msg1 = serde_json::json!({
        "reason": "compiler-artifact",
        "package_id": tokio_id,
        "filenames": [rlib1],
        "features": ["rt"],
        "profile": {"opt_level": "0", "debug_assertions": true, "overflow_checks": true, "test": false}
    });
    let msg2 = serde_json::json!({
        "reason": "compiler-artifact",
        "package_id": tokio_id,
        "filenames": [rlib2],
        "features": ["rt"],
        "profile": {"opt_level": "0", "debug_assertions": true, "overflow_checks": true, "test": false}
    });

    let mut plan = SessionPlan::default();
    let res = plan.add_tokio_artifacts_from_cargo_json(
        &metadata,
        format!("{msg1}\n{msg2}\n").as_bytes(),
        None,
    );
    assert!(
        res.is_err(),
        "multiple artifacts for same package/target/profile must fail registration"
    );
    let err = res.unwrap_err().to_string();
    assert!(err.contains("multiple Cargo artifacts match Tokio package"));
}

#[test]
fn test_h3_tokio_package_name_differs_from_bin_target() {
    use cargo_instrument::session::{CargoDepEdge, R4Profile, SessionPlan, TokioArtifact};
    use std::collections::HashMap;

    let temp = tempfile::tempdir().expect("tempdir");
    let app_dir = temp.path().join("my-service");
    std::fs::create_dir_all(app_dir.join("src/bin")).unwrap();
    let bin_src = app_dir.join("src/bin/server.rs");
    std::fs::write(&bin_src, "fn main() {}\n").unwrap();

    let rlib = temp.path().join("libtokio.rlib");
    std::fs::write(&rlib, b"tokio rlib").unwrap();

    let app_id = "my-service 0.1.0 (path+file:///my-service)";
    let tokio_id = "registry+https://example.invalid#index#tokio@1.43.0";

    let mut package_manifest_dirs = HashMap::new();
    package_manifest_dirs.insert(app_id.to_string(), app_dir.clone());

    let mut package_names_by_id = HashMap::new();
    package_names_by_id.insert(app_id.to_string(), "my-service".to_string());
    package_names_by_id.insert(tokio_id.to_string(), "tokio".to_string());

    let mut package_dependencies = HashMap::new();
    package_dependencies.insert(
        app_id.to_string(),
        vec![CargoDepEdge {
            binding_name: "tokio".to_string(),
            package_id: tokio_id.to_string(),
            kinds: vec![None],
        }],
    );

    let mut tokio_package_by_dependency = HashMap::new();
    tokio_package_by_dependency.insert(app_id.to_string(), tokio_id.to_string());

    let tokio_artifact = TokioArtifact {
        package_id: tokio_id.to_string(),
        package_version: "1.43.0".to_string(),
        resolved_features: vec!["rt".to_string()],
        target: None,
        profile: R4Profile::default(),
        rlib_path: rlib.clone(),
    };

    let plan = SessionPlan {
        package_manifest_dirs,
        package_names_by_id,
        package_dependencies,
        tokio_package_by_dependency,
        tokio_artifacts: vec![tokio_artifact],
        ..Default::default()
    };

    let tokio_args = vec![
        "--crate-name".to_string(),
        "server".to_string(), // rustc crate name differs from Cargo package name 'my-service'
        "--extern".to_string(),
        format!("tokio={}", rlib.display()),
    ];

    assert!(
        plan.unit_has_real_tokio_binding(&bin_src, &tokio_args),
        "package identity from source longest-prefix match must succeed regardless of rustc crate name"
    );
}

#[test]
fn test_h3_live_cargo_build_tokio_feature_safety() {
    use std::fs;
    use std::process::Command;

    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let workspace_root = temp_dir.path();

    // 1. Create mock tokio crate with configurable features (rt, sync)
    let tokio_dir = workspace_root.join("tokio");
    fs::create_dir_all(tokio_dir.join("src")).expect("create tokio/src");
    fs::write(
        tokio_dir.join("Cargo.toml"),
        r#"[package]
name = "tokio"
version = "1.43.0"
edition = "2021"

[features]
default = []
rt = []
sync = []
"#,
    )
    .expect("write tokio Cargo.toml");

    fs::write(
        tokio_dir.join("src/lib.rs"),
        r#"pub fn spawn<F>(f: F)
where
    F: Send + 'static,
{
    let _ = f;
}
"#,
    )
    .expect("write tokio src/lib.rs");

    // 2. Create app crate depending on tokio with features = ["sync"] (lacks "rt")
    let app_dir = workspace_root.join("app");
    fs::create_dir_all(app_dir.join("src")).expect("create app/src");
    fs::write(
        app_dir.join("Cargo.toml"),
        r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { path = "../tokio", default-features = false, features = ["sync"] }
"#,
    )
    .expect("write app Cargo.toml");

    fs::write(
        app_dir.join("src/main.rs"),
        r#"fn work() -> i32 { 100 }
fn main() {
    let w = work();
    assert_eq!(w, 100);
    tokio::spawn(async { 42 });
}
"#,
    )
    .expect("write app src/main.rs");

    let cargo_instrument_bin = env!("CARGO_BIN_EXE_cargo-instrument");
    let target_dir = workspace_root.join("target").join("instrumented");

    // 3. Build with cargo-instrument as RUSTC_WRAPPER
    // Note: in wrapper mode, pre-pass was not run so tokio_artifacts is empty;
    // to test artifact feature validation, we also run cargo-instrument via CLI or directly simulate session plan
    let output = Command::new("cargo")
        .arg("build")
        .arg("--target-dir")
        .arg(&target_dir)
        .current_dir(&app_dir)
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

    // 4. Verify binary compiles and runs
    let bin_name = if cfg!(windows) { "app.exe" } else { "app" };
    let app_bin = target_dir.join("debug").join(bin_name);
    let run_output = Command::new(&app_bin)
        .output()
        .expect("execute compiled app binary");
    assert!(
        run_output.status.success(),
        "compiled app binary failed to run! stderr: {}",
        String::from_utf8_lossy(&run_output.stderr)
    );
}
