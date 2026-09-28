# Dependency Instrumentation Demo

This application configures an in-memory OpenTelemetry exporter and creates an
explicit application span. The dependency wrapper adds spans inside `census`
without changing its source or manifest. The demo checks selected dependency
spans, their parentage, and fallback handle cleanup.

From the repository root, using stable Rust:

```sh
cargo run -p cargo-instrument --bin cargo-instrument -- --with-dependencies -- run --manifest-path examples/demo_app/Cargo.toml
```

Native emission is preferred when compatible artifacts are available. The
application also references `otel_shim::init()` to support the synchronous
fallback. The output does not establish which emitter handled every dependency;
set `INSTRUMENT_DEBUG=1` to inspect wrapper selection diagnostics.

Running the application with ordinary Cargo does not instrument `census` and
will fail the demo's dependency-span assertions. This example does not require
the first-party HIR driver or a source-apply step.
