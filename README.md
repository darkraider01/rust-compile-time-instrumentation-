# Rust Compile-Time Instrumentation

[![CI](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/workflows/ci.yml/badge.svg)](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/workflows/ci.yml)
[![Integration](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/workflows/integration.yml/badge.svg)](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/workflows/integration.yml)
[![License: Apache 2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

Generate OpenTelemetry tracing instrumentation for Rust applications and their dependencies.

## Overview

This project generates spans using the existing [OpenTelemetry Rust API and SDK](https://github.com/open-telemetry/opentelemetry-rust). Applications configure their own tracer provider, sampling, and exporters.

Two workflows cover application code and dependencies:

- **Application code:** `cargo instrument-rust --apply` adds instrumentation directly to eligible first-party source through compiler diagnostics and Cargo fix. Review and commit the generated edits, then build the application normally.
- **Dependencies:** `cargo instrument --with-dependencies -- build` instruments eligible third-party dependencies through a compiler wrapper and isolated source mirrors. Original dependency source and manifests remain unchanged.

Existing explicit instrumentation takes precedence. Native async instrumentation attaches context during polling, and supported Tokio spawn calls capture the caller's context when dispatching a task.

This is an independent prototype. It is not an official OpenTelemetry component.

## Project Status

The core implementation and hardening work are complete. Evaluation against real applications is the next step; production readiness has not been established.

| Component | Current status |
| --- | --- |
| First-party source application | Implemented for supported HIR function forms; repository development workflow |
| Dependency wrapper | Implemented for eligible path and registry dependencies |
| Native dependency emission | Preferred when Cargo artifact compatibility can be proven |
| C ABI runtime | Synchronous fallback when native emission is unavailable and a shim provider is reachable |
| Async dependency lifecycle | Native completion, cancellation, and unwind outcomes |
| Tokio spawn propagation | Implemented for recognized calls in validated native units |
| Preview command and driver distribution | Pending |
| Real-application evaluation | Pending |

Instrumentation currently generates **traces**. Automatic metrics and logs are outside the implemented scope.

## Getting Started

### Build the tools

Use a development checkout with stable Rust, Cargo, and Git:

```sh
git clone https://github.com/darkraider01/rust-compile-time-instrumentation-.git
cd rust-compile-time-instrumentation-
cargo build -p cargo-instrument --bins
```

The commands below assume this checkout's `target/debug` directory is on your PATH. You can also invoke its binaries by absolute path from your application's directory. On Windows, the binaries have an `.exe` suffix.

Keep the checkout available: the first-party command builds its compiler driver from the repository. Packaged driver discovery is not implemented.

### Instrument application code

Source generation additionally requires the pinned nightly toolchain and compiler components:

```sh
rustup toolchain install nightly-2026-09-09 --component rustc-dev --component rust-src --component llvm-tools-preview
```

The pin is recorded in [tools/p23-toolchain.txt](tools/p23-toolchain.txt). See the [compiler driver setup](tools/cargo-instrument-rust-driver/README.md).

In your application, declare a compatible `opentelemetry` dependency and configure an SDK tracer provider and exporter. The current implementation is developed against OpenTelemetry 0.32.0.

Run from a clean Git worktree:

```sh
cargo instrument-rust --apply
# Or select a workspace package:
cargo instrument-rust --apply --package my-app
```

Review the source diff and commit it before applying again. Ownership markers make committed generated code idempotent. Generated application code builds on stable Rust; nightly is required for the source-generation driver.

### Add dependency instrumentation

After reviewing and committing the first-party edits:

```sh
cargo instrument --with-dependencies -- build
cargo instrument --with-dependencies -- run -- <application arguments>
```

Workspace members and the telemetry/executor runtime closure are excluded from dependency mirror instrumentation. Native emission requires compatible Cargo-produced OpenTelemetry artifacts. The synchronous fallback requires the `otel-shim` runtime in the application's graph and a genuine Rust reference such as `otel_shim::init()`; see the [example application](examples/demo_app/src/main.rs).

`CARGO_INSTRUMENT_DEPENDENCIES=1` selects the same dependency policy. Builds use an isolated artifact directory, defaulting to `target/instrumented`.

### Inspect source

From the tool checkout:

```sh
cargo run -p cargo-instrument --bin cargo-instrument -- analyze cargo-instrument/tests/fixtures/census_lib.rs
cargo run -p cargo-instrument --bin cargo-instrument -- transform cargo-instrument/tests/fixtures/census_lib.rs
```

`analyze` reports syntactic candidates and exclusions. `transform` prints a source transformation preview; it does not provide the first-party HIR command's planned `--show` workflow.

## Overview of Crates

| Crate | Purpose |
| --- | --- |
| [cargo-instrument](cargo-instrument) | Cargo commands, source analysis, dependency mirroring, artifact acquisition, and emission |
| [instrument-semantics](instrument-semantics) | Shared eligibility policy and instrumentation semantics |
| [otel-shim](otel-shim) | OpenTelemetry runtime for the C-unwind fallback |
| [cargo-instrument-rust-driver](tools/cargo-instrument-rust-driver) | Separate compiler HIR driver, excluded from the stable workspace |

## Compatibility and Limitations

- The stable workspace tracks stable Rust. No minimum supported Rust version policy is declared. The HIR driver uses the pinned nightly toolchain.
- CI includes Linux, Windows, and macOS. This does not establish compatibility with every target, linker, or application configuration.
- Native dependency artifact acquisition supports ordinary `build` and `run`. Explicit `--target`, `--all-targets`, `--tests`, and `--benches` disable that orchestration path.
- Native selection checks package identity, features, target, profile, and artifact paths. Ambiguous or incompatible units use the available fallback; units without a safe route compile without instrumentation.
- C ABI fallback skips unsupported async sites and does not add Tokio spawn propagation.
- Tokio recognition is conservative: supported qualified paths are recognized, while aliases, shadowed bindings, and missing artifact proof suppress rewriting.
- Macro-owned code, unsupported function forms, and Stream/Sink polling methods are excluded. Instrumentation coverage is not guaranteed for every function.
- Generated spans add runtime work. Instrumented dependency builds also incur wrapper and artifact-acquisition costs.

The completed scale fixture measured **53.67% clean-build overhead** against its baseline, within the approved 55% fixture budget. This is a build-time result for that fixture, not a runtime-overhead estimate or a general performance guarantee. The reproducible workload is in [bench_scale.rs](cargo-instrument/benches/bench_scale.rs).

## Implementation Guides

- [Dependency build orchestration](docs/implementation/dependency-build-orchestration.md)
- [Async context propagation](docs/implementation/async-context-propagation.md)
- [Tokio spawn propagation](docs/implementation/tokio-spawn-propagation.md)
- [Architecture decisions ADR-001–ADR-006](docs/decisions/adr-001-006.md)
- [Architecture decisions ADR-007–ADR-013](docs/decisions/adr-007-013.md)

## Development

Run workspace checks and benchmarks from the checkout:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo bench -p cargo-instrument --bench bench_overhead
cargo bench -p cargo-instrument --bench bench_scale
```

Some subprocess, topology, and registry scenarios are gated behind `--ignored` or environment flags. Consult the relevant test and workflow before running them. The compiler driver has separate toolchain requirements.

See the [CI](.github/workflows/ci.yml), [integration](.github/workflows/integration.yml), and [registry E2E](.github/workflows/e2e.yml) workflows for automated coverage.

## Contributing

Bug reports and implementation feedback are welcome through [GitHub issues](https://github.com/darkraider01/rust-compile-time-instrumentation-/issues). Include the toolchain, platform, Cargo command, and a minimal reproduction when reporting an instrumentation defect.

Architecture changes should update the relevant decision record and include evidence for the change. Evaluation contributions are especially useful for real-application coverage, async behavior, runtime cost, binary size, and build-cache behavior.

## License

[Apache License, Version 2.0](LICENSE).
