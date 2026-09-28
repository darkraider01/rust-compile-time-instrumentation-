use std::sync::{Arc, Barrier, OnceLock};
use std::thread;
use std::time::Instant;

use opentelemetry::trace::{Span as _, Tracer as _};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use serial_test::serial;

#[test]
#[serial]
fn test_tracer_acquisition_latency_profile() {
    const ITERATIONS: usize = 50_000;
    for sdk in [false, true] {
        if sdk {
            opentelemetry::global::set_tracer_provider(SdkTracerProvider::builder().build());
        } else {
            opentelemetry::global::set_tracer_provider(
                opentelemetry::trace::noop::NoopTracerProvider::new(),
            );
        }
        let cached = OnceLock::new();
        cached.get_or_init(|| opentelemetry::global::tracer("profile_crate"));
        let mut uncached_ns = Vec::new();
        let mut cached_ns = Vec::new();
        for sample in 0..10 {
            let measure = |use_cache: bool| {
                let start = Instant::now();
                for _ in 0..ITERATIONS {
                    if use_cache {
                        std::hint::black_box(cached.get().unwrap());
                    } else {
                        std::hint::black_box(opentelemetry::global::tracer("profile_crate"));
                    }
                }
                start.elapsed().as_secs_f64() * 1e9 / ITERATIONS as f64
            };
            let (uncached, cached_value) = if sample % 2 == 0 {
                (measure(false), measure(true))
            } else {
                let cached_value = measure(true);
                (measure(false), cached_value)
            };
            uncached_ns.push(uncached);
            cached_ns.push(cached_value);
        }
        println!(
            "Tracer acquisition sdk={sdk}: uncached ns={uncached_ns:?}, cached ns={cached_ns:?}"
        );
    }
    opentelemetry::global::set_tracer_provider(
        opentelemetry::trace::noop::NoopTracerProvider::new(),
    );
}
#[test]
#[serial]
fn test_dynamic_tracer_provider_invalidation_hazard() {
    opentelemetry::global::set_tracer_provider(
        opentelemetry::trace::noop::NoopTracerProvider::new(),
    );
    let stale_candidate = OnceLock::new();
    let early_tracer =
        stale_candidate.get_or_init(|| opentelemetry::global::tracer("stale_hazard_scope"));
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    opentelemetry::global::set_tracer_provider(provider.clone());
    let mut span = early_tracer.start("early_cached_span");
    span.end();
    let fresh_tracer = opentelemetry::global::tracer("stale_hazard_scope");
    let mut span_fresh = fresh_tracer.start("fresh_acquired_span");
    span_fresh.end();
    provider.force_flush().expect("flush spans");

    let spans = exporter.get_finished_spans().unwrap();
    let names: Vec<_> = spans.iter().map(|s| s.name.as_ref()).collect();
    println!("[Tracer Invalidation Hazard] Exported spans: {:?}", names);
    assert!(
        names.contains(&"fresh_acquired_span"),
        "Fresh tracer must export span"
    );
    let cached_routed = names.contains(&"early_cached_span");
    assert!(
        !cached_routed,
        "Cached tracer must retain its original provider"
    );
}

#[test]
#[serial]
fn test_concurrent_multithreaded_tracer_acquisition() {
    let threads = 8;
    let iterations_per_thread = 2000;
    let barrier = Arc::new(Barrier::new(threads));
    let mut handles = Vec::new();

    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    opentelemetry::global::set_tracer_provider(provider.clone());

    for t in 0..threads {
        let b = Arc::clone(&barrier);
        let scope_name = format!("concurrent_crate_{t}");
        handles.push(thread::spawn(move || {
            b.wait();
            for i in 0..iterations_per_thread {
                let tracer = opentelemetry::global::tracer(scope_name.clone());
                let mut span = tracer.start(format!("span_{t}_{i}"));
                span.set_attribute(opentelemetry::KeyValue::new("thread_id", t as i64));
                span.end();
            }
        }));
    }

    for h in handles {
        h.join().expect("thread join");
    }

    provider.force_flush().expect("flush spans");
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(
        spans.len(),
        threads * iterations_per_thread,
        "Every thread must record and export all spans without loss"
    );
    for t in 0..threads {
        let expected_scope = format!("concurrent_crate_{t}");
        let matching = spans
            .iter()
            .filter(|s| s.instrumentation_scope.name() == expected_scope)
            .count();
        assert_eq!(
            matching, iterations_per_thread,
            "Unexpected scope attribution for thread {t}"
        );
    }
}
