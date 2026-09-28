<div align="center">

# rust-compile-time-instrumentation

**Zero-code compile-time OpenTelemetry instrumentation for Rust reaching third-party dependencies, on stable Rust.**

[![CI](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/workflows/ci.yml/badge.svg)](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/workflows/ci.yml)
[![Integration](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/workflows/integration.yml/badge.svg)](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/workflows/integration.yml)
[![Rust](https://img.shields.io/badge/rust-stable-orange?logo=rust)](https://www.rust-lang.org)
[![Stable output](https://img.shields.io/badge/instrumented%20source-stable%20Rust-brightgreen)](#implementation-guides)
[![License: Apache 2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Phase](https://img.shields.io/badge/phase-2%20complete-brightgreen)](#status)

</div>

Instruments Rust applications *and their dependencies* at build time - no source annotations, no manual span wiring - by intercepting `rustc` via `RUSTC_WRAPPER` and splicing native OpenTelemetry calls at the byte level. The existing wrapper path and already-instrumented application source build on stable Rust. P2.3 source generation intentionally uses an isolated pinned nightly `rustc_private` driver with `rustc-dev`; it does not change the stable workspace toolchain or require nightly to compile generated source.

## Status

| Phase | Status | Focus |
| --- | --- | --- |
| **Architecture Decisions** | **Recorded** | ADR-001–ADR-006 ([decisions](docs/decisions/adr-001-006.md)); ADR-007–ADR-013 ([decisions](docs/decisions/adr-007-013.md)) |
| **Phase 1 - `cargo-instrument` Tool** | **Complete** | Stable Rust compile-time instrumentation pipeline: P1.1–P1.8 complete (end-to-end registry instrumentation, universal AST reconciliation, Cargo 5-pass correctness, and overhead benchmarks verified across the automated suite) |
| **Phase 2 - Production Hardening** | **Complete** | P2.1–P2.5 complete, including large graph validation, the approved 55% clean-build overhead budget (53.67% measured in the latest benchmark), and Windows/Linux/macOS certification. |
| **Phase 3 - Evaluation & Research** | **Planned** | Empirical evaluation: overhead, binary size, async correctness, build-cache behavior, comparison against existing approaches |

## Project Phases

The project is developed incrementally, with each phase establishing and validating a specific part of the compile-time instrumentation pipeline.

### Phase 0 - Architecture Decisions
**Status:** COMPLETE (Frozen)

**Goal:** Establish and record the architecture for compile-time OpenTelemetry instrumentation in Rust.

The six frozen architecture decisions are documented in [ADR-001–ADR-006](docs/decisions/adr-001-006.md).

---

### Phase 1 - `cargo-instrument` Tool
**Status:** COMPLETE

**Goal:** Build and validate the compile-time instrumentation pipeline on stable Rust.

- [x] **P1.1 - Cargo / `RUSTC_WRAPPER` interception** - COMPLETE
  Intercepts Cargo's `rustc` invocations, preserves arguments and exit codes, detects direct nested wrapper invocations, and enforces isolated build artifact directories (`target/instrumented`, [ADR-004](docs/decisions/adr-001-006.md)).
- [x] **P1.2 - Source discovery & compilation-unit classification** - COMPLETE
  Classifies compiler invocations (ordinary crate, build script, proc macro, compiler queries, pass-through), extracts primary source files, and parses compiler options without modifying inputs.
- [x] **P1.3 - `syn` AST & exact byte-span analysis** - COMPLETE
  Performs full `syn` AST parsing, root-aware recursive module discovery (`mod foo;`), identifies eligible function items (free functions, inherent methods, trait methods), filters exclusions (`const fn`, `extern "C"`, nested functions, direct self-recursion), applies R10 idempotence heuristics (closure-based `with_context` discrimination, `.start()` checks), and calculates exact UTF-8 byte ranges (`start..end`) while keeping original source files byte-for-byte untouched.
- [x] **P1.4 - Surgical source transformation** - COMPLETE
  Transforms original UTF-8 source buffers via deterministic single-pass byte splicing without modifying input files in-place (ADR-002, S1/S2). Features exact normalized path filtering (C1), S11 candidate-level fail-open skips (H2), a pluggable emitter seam (ADR-006 / H3), source line-ending preservation (M1), structurally constrained idempotence (M3), and full live integration into the compiler wrapper pipeline.
- [x] **P1.5 - Native OpenTelemetry code generation** - COMPLETE
  Generates native OpenTelemetry 0.32.0 API calls for synchronous functions with zero dependencies on tracing abstractions, handling tracer acquisition per-crate (`opentelemetry::global::tracer("{crate_name}")`), RAII context attachment, Result error recording with pinned `Result<_, _>`, clippy-clean closure wrapping, `--extern` dependency gating with S11 fail-open, and normalized span naming.
- [x] **P1.6 - Async instrumentation** - COMPLETE
  Instruments async functions using `opentelemetry::trace::FutureExt::with_context`, preserving trace context across future suspension points and multi-threaded executor task migration without holding `!Send` guards. Features Send-bound preservation, `#[async_trait]` compatibility, cancellation-on-drop span export, post-await Result error status recording, wall-clock duration measurement (§16.7), and zero clippy warnings.
- [x] **P1.7 - Dependency instrumentation / `extern "C"` trampolines** - COMPLETE
  Extends instrumentation across third-party Cargo crate boundaries without manifest mutation or Cargo dependency injection using `extern "C"` ABI trampolines (`__otel_span_enter`, `__otel_span_exit`, `__otel_span_set_error`). Includes standalone `otel-shim` runtime crate exporting C ABI on native OpenTelemetry SDK with thread-local LIFO matching, compile-time application preflight checking (`otel_shim::init()`) to prevent extern-crate pruning (ADR-003 / E-10), edition 2021 vs 2024 awareness, `UnsafePolicy` handling, and live multi-threaded end-to-end integration proof.
- [x] **P1.8 - End-to-end validation** - COMPLETE
  Validated emitted spans on real crates.io dependencies (`census = "=0.4.2"`) with cross-crate parenting and independently selected `cesu8 = "=1.1.0"`; proved registry source cache immutability, verified Cargo correctness across five passes, tested safety negatives and sandboxing, and measured clean, repeat, and incremental build overhead.

---

### Phase 2 - Production Hardening
**Status:** COMPLETE

**Goal:** Establish the reliability, usability, and scale required for production build environments.

- [x] **P2.1 - Unit Identity, Instrumentation Policy & Mirror Isolation** - COMPLETE
  Introduces unique compilation unit identity (`UnitId`) parsed from `-C metadata` in rustc argv, isolates instrumented source mirrors per unit (`{crate_name}-{metadata_hash}`), performs atomic mirror writes with fail-open fallback, enforces build session policy via single-pass `cargo metadata` resolution (host-only package exclusion and link-provider reachability gating), scopes `.d` dep-info remapping, and downgrades preflight checks to non-fatal warnings.
- [x] **P2.2 - Macro Expansion Resilience & Coexistence** - COMPLETE
  Establishes clean coexistence between automatic instrumentation and developer-written annotations ([ADR-009](docs/decisions/adr-007-013.md#adr-009---explicit-instrumentation-wins-at-whole-function-granularity)). Widens the explicit-instrumentation matcher to any qualified path (`#[tracing::instrument]`, `#[tracing_attributes::instrument]`, `#[otel_instrument::instrument]`, `#[propagate_context]`), skips such functions whole to prevent duplicate spans and `__otel_cx` identifier shadowing, and proves hybrid parenting - an explicit `#[tracing::instrument]` caller adopting automatically instrumented dependency spans as children - across both synchronous and `#[async_trait]` boundaries ([ADR-010](docs/decisions/adr-007-013.md#adr-010---hybrid-parenting-is-delegated-to-tracing-opentelemetry)). Measured over-suppression on `census-0.4.2` and `async-trait`: 0.0%.
- [x] **P2.3 - First-Party Lint-Apply Driver (`cargo instrument-rust`)** - SEMANTIC INSTRUMENTATION COMPLETE
  `cargo instrument-rust --apply` clean-tree-gates a first-party-only `cargo fix` run, puts the isolated nightly `rustc_driver` HIR frontend in `RUSTC`, and leaves Cargo's `RUSTC_WRAPPER` diagnostics proxy intact. It emits genuine `MachineApplicable` edits for supported semantic forms: ordinary free functions, inherent methods, trait implementation methods, native `async fn` bodies, and verified `#[async_trait]` methods when `opentelemetry` is present. Result status recording captures `Status::error("")` for semantic `core::result::Result` types, explicit user instrumentation takes precedence, and direct self-recursion on `self` is excluded. Deliberate exclusions: nested local functions, default trait method bodies, macro/expansion-owned source, const functions, closures, and foreign ABIs. The persistent marker `/* __cargo_instrument_rust:p23 */` makes the command idempotent. The process fixture proves edit, dependency-source immutability, stable rebuild, dirty-tree refusal, and a committed no-op second run. The stable workspace does not depend on `rustc_private`; the driver requires nightly plus `rustc-dev`. Operational packaging and preview UX (`--show`) remain deliberate follow-ups.
- [x] **P2.4 - Opt-In Dependency Pipeline & Async Trampolines** - COMPLETE
  Exposes `--with-dependencies` and completes the **Hybrid Fallback** architecture: native `--extern` injection for compatible units, with the synchronous Tier-2 C ABI retained as fallback. See the [dependency build orchestration](docs/implementation/dependency-build-orchestration.md), [async context propagation](docs/implementation/async-context-propagation.md), and [Tokio spawn propagation](docs/implementation/tokio-spawn-propagation.md) guides.
- [x] **P2.5 - Large Dependency Graphs & Cross-Platform Validation** - COMPLETE
  Validates both hybrid modes across large multi-crate workspaces and ≥100-unit dependency graphs. Records the approved 55% clean-build overhead budget (53.67% measured in the latest benchmark), confirms dynamic tracer lookup preserves provider replacement semantics, and certifies Windows (MSVC), Linux (ELF), and macOS (Mach-O).

---

### Phase 3 - Evaluation & Research
**Status:** PLANNED

**Goal:** Conduct an empirical engineering evaluation comparing compile-time instrumentation against existing paradigms.

Planned evaluation:
- **Instrumentation coverage:** Quantify percentage of application and dependency call sites successfully captured.
- **Compile-time overhead:** Measure build-time impact across cold, incremental, and clean builds.
- **Runtime overhead:** Benchmark throughput and latency impact of injected native spans.
- **Binary size:** Measure stripped and unstripped binary size delta from trampolines and OTel symbols.
- **Build-cache behavior:** Verify cache hit rates under Cargo and CI caching tools (`sccache`, `rust-cache`).
- **Async correctness:** Verify context propagation correctness under high async concurrency and task cancellation.
- **Dependency coverage:** Evaluate capture depth across third-party crate boundaries.
- **Comparison against existing approaches:** Systematic comparison with manual instrumentation (`tracing`), macro-based approaches, and eBPF kernel tracing.

---

### Current Focus: Phase 2 - Production Hardening

Phase 1 (Milestones P1.1–P1.8) and Phase 2 Milestones P2.1–P2.5 are **COMPLETE**.

Architecture decisions [ADR-011](docs/decisions/adr-007-013.md#adr-011---the-tier-2-c-abi-is-provisional), [ADR-012](docs/decisions/adr-007-013.md#adr-012---hybrid-first-partydependency-instrumentation-architecture), and [ADR-013](docs/decisions/adr-007-013.md#adr-013---p23p24-architecture-freeze-and-cargo-fix-integration) establish the hybrid architecture: first-party lint-apply (`cargo instrument-rust`) is the default workflow, while compile-time wrapper dependency instrumentation remains opt-in.

## Implementation Guides

- [Dependency build orchestration](docs/implementation/dependency-build-orchestration.md)
- [Async context propagation](docs/implementation/async-context-propagation.md)
- [Tokio spawn propagation](docs/implementation/tokio-spawn-propagation.md)
- [Architecture decisions ADR-001–ADR-006](docs/decisions/adr-001-006.md)
- [Architecture decisions ADR-007–ADR-013](docs/decisions/adr-007-013.md)

## CLI Usage & Prototype Demonstration

### P2.4 dependency opt-in

First-party source uses `cargo instrument-rust --apply`. After reviewing and
committing those edits, build or run with dependency instrumentation:

```text
cargo instrument --with-dependencies -- build
cargo instrument --with-dependencies -- run -- <application arguments>
```

For a development checkout, invoke the binary with
`cargo run --bin cargo-instrument -- --with-dependencies -- build`.
`CARGO_INSTRUMENT_DEPENDENCIES=1` selects the same policy. Workspace members and
the telemetry/executor runtime closure are excluded from mirror instrumentation.
Native-compatible dependencies support async lifecycle outcomes and Tokio context
propagation; the required C-ABI fallback handles synchronous functions and skips
unsupported async sites. Stream/Sink polling methods are excluded to avoid a span
per poll/item. The implementation details are in the dependency and async guides
linked above.

`cargo-instrument` operates in two primary modes:
1. **Interactive CLI**: Standalone AST inspection (`analyze`) and surgical transformation preview (`transform`).
2. **Transparent Compiler Driver**: Invoking Cargo with `cargo run --bin cargo-instrument -- -- <cargo args...>` wraps `rustc` via `RUSTC_WRAPPER` and automatically routes build artifacts to an isolated directory (`target/instrumented`, per [ADR-004](docs/decisions/adr-001-006.md)).

### 1. Legacy wrapper compatibility demonstration

Compiles and executes a sample application ([`examples/demo_app`](examples/demo_app/src/main.rs)) that calls into third-party dependency `census = "=0.4.2"`, automatically exporting 26 OpenTelemetry spans with cross-crate trace parenting and zero handle leaks:

**PowerShell (Windows):**
```powershell
$env:CARGO_INSTRUMENT_REGISTRY="1"; cargo run --bin cargo-instrument -- -- run --manifest-path examples/demo_app/Cargo.toml
```

**Bash (Linux / macOS):**
```bash
CARGO_INSTRUMENT_REGISTRY=1 cargo run --bin cargo-instrument -- -- run --manifest-path examples/demo_app/Cargo.toml
```

### 2. AST Candidate Analysis (`analyze`)

Parses Rust source code, identifies eligible function items, categorizes exclusions (adapter traits, Drop impls, tests, recursion), and reports universal reconciliation statistics:

```bash
# Analyze the checked-in census crate fixture (portable, offline)
cargo run --bin cargo-instrument -- analyze cargo-instrument/tests/fixtures/census_lib.rs
```

To analyze a cached crates.io dependency directly:
- **PowerShell (Windows):**
  ```powershell
  cargo run --bin cargo-instrument -- analyze (Resolve-Path "$env:USERPROFILE\.cargo\registry\src\index.crates.io-*\census-0.4.2\src\lib.rs").Path
  ```
- **Bash (Linux / macOS):**
  ```bash
  cargo run --bin cargo-instrument -- analyze ~/.cargo/registry/src/index.crates.io-*/census-0.4.2/src/lib.rs
  ```

### 3. Surgical Source Splicer Preview (`transform`)

Displays the transformed source code with non-destructive, byte-level insertions of RAII OpenTelemetry trampoline guards (`__OtelGuard`):

```bash
cargo run --bin cargo-instrument -- transform cargo-instrument/tests/fixtures/census_lib.rs
```

### 4. Wrapped Cargo Builds (`-- <cargo args>`)

Invokes Cargo while automatically configuring `RUSTC_WRAPPER` and ensuring `target/instrumented` isolation:

```bash
cargo run --bin cargo-instrument -- -- build
cargo run --bin cargo-instrument -- -- check
```

### 5. Automated Test Suites & Overhead Benchmarks

Validate the complete 145-test suite across unit, integration, and registry fixtures, or run empirical benchmarks:

```bash
# Run offline test suite (154 tests pass; 13 network/topology tests gated behind --ignored)
cargo test --workspace

# Run live crates.io registry E2E validation suite
# PowerShell:
$env:CARGO_INSTRUMENT_REGISTRY="1"; cargo test --workspace --test e2e_registry_tests -- --ignored --nocapture
# Bash:
CARGO_INSTRUMENT_REGISTRY=1 cargo test --workspace --test e2e_registry_tests -- --ignored --nocapture

# Run Phase 2 graph topology regression suite (G1-G4, G7, D1-D5)
cargo test --test graph_topology_tests -- --ignored --nocapture

# Run Phase 2 hybrid coexistence suite (explicit + automatic instrumentation, P2.2)
cargo test --test hybrid_instrumentation_tests -- --nocapture

# Run Phase 2 scale fixture test (>=100 units under parallel compilation)
cargo test --test graph_scale_tests -- --nocapture

# Run compile-time, runtime nanosecond latency, and binary size benchmarks (A17)
cargo bench --bench bench_overhead
```

CI runs `fmt`/`clippy`/`test`/`build` across Linux, Windows, and macOS ([`ci.yml`](.github/workflows/ci.yml)), plus a dedicated real-subprocess integration workflow ([`integration.yml`](.github/workflows/integration.yml)) and end-to-end registry workflow ([`e2e.yml`](.github/workflows/e2e.yml)).


## License

[Apache License, Version 2.0](LICENSE).
