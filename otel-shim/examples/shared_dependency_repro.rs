//! Live demo of the C1/C2 fix: what happens when the APPLICATION and the
//! OTel EXPORTER call into the exact same compile-time-instrumented function,
//! because they both depend on the same instrumented crate (e.g. hyper).
//!
//! Run it:
//!   cargo run --example shared_dependency_repro -p otel-shim

use opentelemetry_sdk::trace::{SdkTracerProvider, SpanData, SpanExporter};

/// Stand-in for a function inside a shared dependency (e.g. `hyper::Client::request`)
/// that `cargo-instrument` has compile-time-instrumented. BOTH the application code
/// and the OTLP exporter call this same function, because both link the same
/// instrumented HTTP client crate -- that's the "shared instrumented dependency"
/// scenario.
fn shared_instrumented_http_call(caller: &str) {
    println!("  [{caller}] -> calling into the shared instrumented dependency (e.g. hyper)...");
    let name = b"hyper::Client::request";
    let file = b"hyper/src/client.rs";
    unsafe {
        let h = otel_shim::__otel_span_enter(
            name.as_ptr(),
            name.len(),
            file.as_ptr(),
            file.len(),
            42,
            0,
        );
        if h == 0 {
            println!(
                "  [{caller}] -> __otel_span_enter returned 0 -> RE-ENTRANCY CAUGHT, no nested span created"
            );
        } else {
            println!("  [{caller}] -> __otel_span_enter returned handle {h} -> new span created");
        }
        otel_shim::__otel_span_exit(h);
    }
}

/// An exporter shaped like a real OTLP exporter: it sends spans over the network
/// using the same instrumented HTTP client the application uses.
#[derive(Debug)]
struct OtlpLikeExporter;

impl SpanExporter for OtlpLikeExporter {
    async fn export(&self, batch: Vec<SpanData>) -> opentelemetry_sdk::error::OTelSdkResult {
        println!(
            "[exporter] sending {} span(s) to the collector...",
            batch.len()
        );
        // The exporter needs to make an HTTP call to ship the span, and that
        // HTTP call goes through the SAME instrumented dependency the app uses.
        shared_instrumented_http_call("exporter");
        println!("[exporter] send complete.");
        Ok(())
    }
}

fn main() {
    otel_shim::init();

    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(OtlpLikeExporter)
        .build();
    opentelemetry::global::set_tracer_provider(provider);

    println!("=== application code makes a request through the shared instrumented dependency ===");
    shared_instrumented_http_call("app");

    println!();
    println!(
        "=== done: process exited normally (no panic, no deadlock) after the exporter \
         re-entered the same instrumented function mid-export ==="
    );
}
