# Dependency Instrumentation Demo

The demo uses the real registry crate `census` for synchronous cross-crate
parenting and a separate local dependency for async context propagation and
Tokio task propagation. The local async dependency has its own Cargo workspace,
so the wrapper treats it as unowned dependency source and instruments it without
editing its source or manifest.

From the repository root, using stable Rust:

```sh
cargo run -p cargo-instrument --bin cargo-instrument -- --with-dependencies -- run --manifest-path examples/demo_app/Cargo.toml --offline
```

The app creates explicit parent spans, calls an instrumented async dependency
function that suspends, then calls another dependency function that starts a
Tokio task. It checks the exported parent IDs and trace IDs for each path and
asserts that no fallback span handles remain active. With `INSTRUMENT_DEBUG=1`,
the log also shows the selected emitter and transformed dependency units.

This checks functional async context propagation for one controlled workload.
It does not measure runtime overhead or establish behavior across real
applications. The separate [runtime overhead benchmark](../../cargo-instrument/benches/bench_overhead.rs)
compares baseline and instrumented synchronous calls.

The first-party source apply driver is not required. The app's span calls are
explicit because the `--with-dependencies` workflow instruments dependencies,
not workspace application source.
