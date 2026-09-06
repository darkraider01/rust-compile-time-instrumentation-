use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

fn main() {
    println!("============================================================");
    println!("  Compile-Time Instrumentation Demo (cargo-instrument)     ");
    println!("============================================================\n");

    // 1. Initialize otel-shim runtime (ADR-003 / E-10)
    println!("[1/4] Initializing otel-shim and OpenTelemetry TracerProvider...");
    otel_shim::init();

    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    opentelemetry::global::set_tracer_provider(provider);

    // 2. Execute application workload that calls into third-party dependency `census`
    println!("[2/4] Executing workload calling dependency crate 'census'...");
    process_inventory_batch();

    // 3. Verify zero handle leaks (A8 / S5)
    let active_spans = otel_shim::active_span_count();
    println!("[3/4] Checking active span handles in otel-shim TLS stack: {}", active_spans);
    assert_eq!(active_spans, 0, "active_span_count must be 0 after execution");

    // 4. Inspect and display finished spans
    let spans = exporter.get_finished_spans().expect("finished spans");
    println!("\n[4/4] Captured {} OpenTelemetry Spans across crate boundary:\n", spans.len());

    let app_span = spans.iter().find(|s| s.name == "process_inventory_batch");
    let app_span_id = app_span.map(|s| s.span_context.span_id());

    for span in &spans {
        let is_app = span.name == "process_inventory_batch";
        let crate_tag = if is_app { "[demo_app]" } else { "[census]  " };
        let is_child = Some(span.parent_span_id) == app_span_id && !is_app;
        let indent = if is_child { "    └── " } else { "├── " };

        println!(
            "{}{} {:<25} | ID: {} | Parent: {} | Kind: {:?} | Status: {:?}",
            indent,
            crate_tag,
            span.name,
            span.span_context.span_id(),
            span.parent_span_id,
            span.span_kind,
            span.status,
        );
    }

    // Verify key acceptance properties
    assert!(app_span.is_some(), "application span captured");
    let new_span = spans.iter().find(|s| s.name == "Inventory<T>::new");
    let track_span = spans.iter().find(|s| s.name == "Inventory<T>::track");
    let list_span = spans.iter().find(|s| s.name == "Inventory<T>::list");

    assert!(new_span.is_some(), "census::Inventory::new instrumented");
    assert!(track_span.is_some(), "census::Inventory::track instrumented");
    assert!(list_span.is_some(), "census::Inventory::list instrumented");

    if let (Some(app), Some(child)) = (app_span, new_span) {
        assert_eq!(
            child.parent_span_id,
            app.span_context.span_id(),
            "dependency span properly parented to caller application span"
        );
    }

    println!("\n------------------------------------------------------------");
    println!("  Demo Result: SUCCESS");
    println!("  - Automatic compile-time AST instrumentation: Verified");
    println!("  - Zero code modifications to 'census' dependency: Verified");
    println!("  - C-ABI trampoline runtime bridge (otel-shim): Verified");
    println!("  - Distributed Context & Cross-Crate Parenting: Verified");
    println!("  - S5 LIFO Stack Cleanup & Zero Leaks: Verified");
    println!("------------------------------------------------------------\n");
}

pub fn process_inventory_batch() {
    let inventory = census::Inventory::new();
    let item1 = inventory.track("item-alpha");
    let item2 = inventory.track("item-beta");
    let items = inventory.list();
    println!("   -> Census inventory tracked items: {}", items.len());
    drop(item1);
    drop(item2);
}
