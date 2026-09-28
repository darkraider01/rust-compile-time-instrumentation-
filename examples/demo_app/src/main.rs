use opentelemetry::trace::{FutureExt, TraceContextExt, Tracer};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    println!("Compile-time dependency instrumentation demo\n");

    // Keep the runtime linked for dependency units that need C-ABI fallback.
    otel_shim::init();

    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    opentelemetry::global::set_tracer_provider(provider);

    process_inventory_batch();
    run_async_dependency_work().await;

    let active_spans = otel_shim::active_span_count();
    assert_eq!(active_spans, 0, "no fallback span handles should remain");

    let spans = exporter.get_finished_spans().expect("read exported spans");
    println!("Captured {} OpenTelemetry spans:", spans.len());
    for span in &spans {
        println!(
            "{} | trace={} | span={} | parent={}",
            span.name,
            span.span_context.trace_id(),
            span.span_context.span_id(),
            span.parent_span_id,
        );
    }

    let sync_parent = find_span(&spans, "process_inventory_batch");
    let census_child = find_span(&spans, "Inventory<T>::new");
    assert_eq!(
        census_child.parent_span_id,
        sync_parent.span_context.span_id(),
        "synchronous dependency call inherits the application span"
    );

    let direct_parent = find_span(&spans, "demo_async_direct_parent");
    let direct_child = find_span(&spans, "async_direct_dependency");
    assert_eq!(
        direct_child.parent_span_id,
        direct_parent.span_context.span_id()
    );
    assert_eq!(
        direct_child.span_context.trace_id(),
        direct_parent.span_context.trace_id()
    );

    let spawn_parent = find_span(&spans, "demo_async_spawn_parent");
    let spawned_function = find_span(&spans, "async_spawn_dependency");
    let spawned_child = find_span(&spans, "async_spawn_child");
    assert_eq!(
        spawned_function.parent_span_id,
        spawn_parent.span_context.span_id()
    );
    assert_eq!(
        spawned_child.parent_span_id,
        spawned_function.span_context.span_id()
    );
    assert_eq!(
        spawned_child.span_context.trace_id(),
        spawn_parent.span_context.trace_id()
    );

    println!("\nDemo Result: SUCCESS");
    println!("- Sync cross-crate parentage: verified");
    println!("- Async dependency context across suspension: verified");
    println!("- Context propagation into Tokio task spawned inside dependency: verified");
    println!("- Active fallback handles after completion: {active_spans}");
}

fn find_span<'a>(
    spans: &'a [opentelemetry_sdk::trace::SpanData],
    name: &str,
) -> &'a opentelemetry_sdk::trace::SpanData {
    spans
        .iter()
        .find(|span| span.name == name)
        .unwrap_or_else(|| panic!("expected exported span `{name}`"))
}

async fn run_async_dependency_work() {
    let tracer = opentelemetry::global::tracer("demo_app");

    let direct_parent =
        opentelemetry::Context::current_with_span(tracer.start("demo_async_direct_parent"));
    let direct_result = async { demo_async_dep::async_direct_dependency().await };
    let direct_result = direct_result.with_context(direct_parent.clone()).await;
    assert_eq!(direct_result, 21);
    direct_parent.span().end();

    let spawn_parent =
        opentelemetry::Context::current_with_span(tracer.start("demo_async_spawn_parent"));
    let spawn_result = async { demo_async_dep::async_spawn_dependency().await };
    let spawn_result = spawn_result.with_context(spawn_parent.clone()).await;
    assert_eq!(spawn_result, 21);
    spawn_parent.span().end();
}

fn process_inventory_batch() {
    let tracer = opentelemetry::global::tracer("demo_app");
    let span = tracer.start("process_inventory_batch");
    let _guard = opentelemetry::Context::current_with_span(span).attach();

    let inventory = census::Inventory::new();
    let item1 = inventory.track("item-alpha");
    let item2 = inventory.track("item-beta");
    let items = inventory.list();
    println!("census tracked {} items", items.len());
    drop(item1);
    drop(item2);
}
