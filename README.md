<div align="center">

# rust-compile-time-instrumentation

**Zero-code compile-time OpenTelemetry instrumentation for Rust reaching third-party dependencies, on stable Rust.**

[![CI](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/workflows/ci.yml/badge.svg)](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/workflows/ci.yml)
[![Integration](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/workflows/integration.yml/badge.svg)](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/workflows/integration.yml)
[![Rust](https://img.shields.io/badge/rust-stable-orange?logo=rust)](https://www.rust-lang.org)
[![No nightly](https://img.shields.io/badge/nightly-not%20required-brightgreen)](docs/research/17-decision-records.md)
[![License: Apache 2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Phase](https://img.shields.io/badge/phase-2%20in%20progress-blue)](#status)

</div>

Instruments Rust applications *and their dependencies* at build time - no source annotations, no manual span wiring - by intercepting `rustc` via `RUSTC_WRAPPER` and splicing native OpenTelemetry calls at the byte level. Runs on stable Rust with no compiler forks, no MIR passes, and no eBPF; the frozen architecture and the evidence behind it are in [`docs/research/`](docs/research/).

## Status

| Phase | Status | Focus |
| --- | --- | --- |
| **Phase 0 - Landscape Research & Architecture** | **Complete** (Frozen) | Six frozen architecture decisions ([ADR-001 … ADR-006](docs/research/17-decision-records.md)), normative correctness spec ([§16](docs/research/16-instrumentation-semantics.md)), experiment matrix ([Appendix E](docs/research/appendix-e-experiment-matrix.md)) |
| **Phase 1 - `cargo-instrument` Tool** | **Complete** | Stable Rust compile-time instrumentation pipeline: P1.1–P1.8 complete (end-to-end registry instrumentation, universal AST reconciliation, Cargo 5-pass correctness, and overhead benchmarks verified across the automated suite) |
| **Phase 2 - Production Hardening** | **In Progress** | Unit identity & mirror isolation (P2.1 complete), macro expansion resilience & coexistence (P2.2 complete), first-party lint-apply driver (P2.3 in progress), async dependency trampolines & opt-in pipeline (P2.4), large graphs & cross-platform validation (P2.5). Decisions recorded as [ADR-007 … ADR-012](docs/phase2/decision-records.md) |
| **Phase 3 - Evaluation & Research** | **Planned** | Empirical evaluation: overhead, binary size, async correctness, build-cache behavior, comparison against existing approaches |

## Project Phases

The project is developed incrementally, with each phase establishing and validating a specific part of the compile-time instrumentation pipeline.

### Phase 0 - Landscape Research & Architecture
**Status:** COMPLETE (Frozen)

**Goal:** Determine whether zero-code compile-time OpenTelemetry instrumentation for Rust is technically viable and establish a defensible, frozen architecture.

**Summary:**
- **Ecosystem & landscape research:** Survey of existing Rust (`tracing`, `autometrics`), Go (`otelc`), and eBPF (`open-telemetry/opentelemetry-ebpf-profiler`, OBI) instrumentation approaches ([§4](docs/research/04-rust-instrumentation-landscape.md), [§8](docs/research/08-competitive-landscape.md)).
- **OpenTelemetry Go `otelc` prior art:** Analysis of `otelc`'s AST splicing, toolchain wrapping, and compatibility strategy ([§2](docs/research/02-otelc-go.md)).
- **Rust/Cargo/compiler experiments:** Empirical testing of rustc compilation phases, AST expansion, macro expansion limits, and `RUSTC_WRAPPER` execution behavior ([§3](docs/research/03-rust-compiler-pipeline.md), [Appendix B](docs/research/appendix-b-verification-log.md)).
- **Maintainer validation:** Direct consultations across OpenTelemetry Rust (`#otel-rust`), Go (`#otel-go`), and eBPF (`#otel-ebpf`) SIGs ([Appendix D](docs/research/appendix-d-maintainer-qa.md)).
- **Native OpenTelemetry API decision:** Adopted native `opentelemetry` API calls (`FutureExt::with_context` for async), eliminating runtime dependencies on `tracing`/`tracing-subscriber` ([ADR-001](docs/research/17-decision-records.md), [Appendix D.2](docs/research/appendix-d-maintainer-qa.md)).
- **eBPF branch closure:** Formally closed research into eBPF-based Tokio async task reconstruction (Architectures C & D) following upstream progress in OBI (#1096), focusing 100% on compile-time Cargo instrumentation for environments where eBPF is unavailable or impractical ([ADR-005](docs/research/17-decision-records.md), [§15.6](docs/research/15-final-recommendation.md)).
- **`extern "C"` trampoline decision:** Standardized on `__otel_span_enter` and `__otel_span_exit` trampolines to enable dependency instrumentation without transitively injecting heavy OTel dependencies into every upstream crate ([ADR-002](docs/research/17-decision-records.md), [§12.1a](docs/research/12-mvp-definition.md)).
- **Surgical byte-range transformation decision:** Selected direct source splicing over AST pretty-printing (`prettyplease`) to guarantee zero comment loss, format preservation, and bit-level determinism ([ADR-003](docs/research/17-decision-records.md)).
- **Architecture & risk documentation:** Established the technical risk register ([§13](docs/research/13-technical-risks.md)), normative instrumentation semantics ([§16](docs/research/16-instrumentation-semantics.md)), and validated experiment matrix ([Appendix E](docs/research/appendix-e-experiment-matrix.md)).

Phase 0 is frozen. All historical records, ADRs, and verification logs are archived under [`docs/research/`](docs/research/).

---

### Phase 1 - `cargo-instrument` Tool
**Status:** COMPLETE

**Goal:** Build and validate the compile-time instrumentation pipeline on stable Rust.

- [x] **P1.1 - Cargo / `RUSTC_WRAPPER` interception** - COMPLETE
  Intercepts Cargo's `rustc` invocations, preserves arguments and exit codes, detects direct nested wrapper invocations, and enforces isolated build artifact directories (`target/instrumented`, [ADR-004](docs/research/17-decision-records.md)).
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
  Validated emitted spans on real crates.io dependencies (`census = "=0.4.2"`) with cross-crate parenting, validated against an independently selected unseen crates.io dependency ([`cesu8 = "=1.1.0"`](docs/phase1/p1.8/validation-report.md)), proved bit-for-bit registry source cache immutability, established universal AST candidate reconciliation identity ($16+16=32$ on `census` as measured at P1.8, $34+21=55$ on `async-trait`, $15+3=18$ on `cesu8`; the `census` split is $18+14=32$ after the P2 [D6](docs/phase2/README.md) recursion-detector fix, with the total unchanged), verified Cargo correctness across 5 passes, proved safety negatives and sandboxing (A15/A16), and recorded overhead benchmarks on clean, repeat, and incremental builds.

---

### Phase 2 - Production Hardening
**Status:** IN PROGRESS

**Goal:** Establish the reliability, usability, and scale required for production build environments.

- [x] **P2.1 - Unit Identity, Instrumentation Policy & Mirror Isolation** - COMPLETE
  Introduces unique compilation unit identity (`UnitId`) parsed from `-C metadata` in rustc argv, isolates instrumented source mirrors per unit (`{crate_name}-{metadata_hash}`), performs atomic mirror writes with fail-open fallback, enforces build session policy via single-pass `cargo metadata` resolution (host-only package exclusion and link-provider reachability gating), scopes `.d` dep-info remapping, and downgrades preflight checks to non-fatal warnings.
- [x] **P2.2 - Macro Expansion Resilience & Coexistence** - COMPLETE
  Establishes clean coexistence between automatic instrumentation and developer-written annotations ([ADR-009](docs/phase2/decision-records.md#adr-009---explicit-instrumentation-wins-at-whole-function-granularity)). Widens the explicit-instrumentation matcher to any qualified path (`#[tracing::instrument]`, `#[tracing_attributes::instrument]`, `#[otel_instrument::instrument]`, `#[propagate_context]`), skips such functions whole to prevent duplicate spans and `__otel_cx` identifier shadowing, and proves hybrid parenting - an explicit `#[tracing::instrument]` caller adopting automatically instrumented dependency spans as children - across both synchronous and `#[async_trait]` boundaries ([ADR-010](docs/phase2/decision-records.md#adr-010---hybrid-parenting-is-delegated-to-tracing-opentelemetry)). Measured over-suppression on `census-0.4.2` and `async-trait`: 0.0%.
- [ ] **P2.3 - First-Party Lint-Apply Driver (`cargo instrument-rust`)** - IN PROGRESS ◀── current
  Implements the default first-party workflow via `rustc_private` (`rustc_lint` and `rustc_errors`) per [ADR-012](docs/phase2/decision-records.md#adr-012---hybrid-first-partydependency-instrumentation-architecture). Generates visible compiler diagnostics (`--show`) and machine-applicable source modifications on disk (`--apply`) gated on a clean git working tree, eliminating recurring build overhead. Feasibility spike (`0b98e5f`) confirmed that body wrapping via `span_to_snippet` is expressible and proc-macro call-site span preservation allows `#[async_trait]` method bodies to be reached directly (`from_expansion = false`).
- [ ] **P2.4 - Opt-In Dependency Pipeline & Async Trampolines** - PLANNED
  Hardens the opt-in dependency instrumentation path. Resolves R-4 ([ADR-011](docs/phase2/decision-records.md#adr-011---the-tier-2-c-abi-is-provisional)): evaluates `--extern` injection pre-passes to eliminate the C ABI, or extends Tier-2 for R-1 (per-crate scope) and R-2 (file/line/kind attributes). Implements async dependency trampolines (`tokio::spawn` context propagation per ADR-001, Stream/Sink poll boundaries, and cancelled-vs-completed span lifecycle).
- [ ] **P2.5 - Large Dependency Graphs & Cross-Platform Validation** - PLANNED
  Validates both hybrid modes across large multi-crate workspaces and ≥100-unit dependency graphs. Implements tracer caching (`OnceLock`) per §16.3, and verifies cross-platform execution on Windows (MSVC with MAX_PATH mitigation), Linux (ELF), and macOS (Mach-O).

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

Phase 1 (Milestones P1.1–P1.8) is **COMPLETE**. Phase 2 Milestones P2.1 (Unit Identity, Instrumentation Policy & Mirror Isolation) and P2.2 (Macro Expansion Resilience & Coexistence) are **COMPLETE**, with 11 graph-topology regression tests covering defects G1–G4, G7, and closeout defects D1–D5, a 105-unit parallel scale fixture, a 5-test hybrid coexistence suite, and 176 tests total across the workspace.

Architecture decisions [ADR-011](docs/phase2/decision-records.md#adr-011---the-tier-2-c-abi-is-provisional) and [ADR-012](docs/phase2/decision-records.md#adr-012---hybrid-first-partydependency-instrumentation-architecture) establish a hybrid architecture: first-party lint-apply (`cargo instrument-rust`) becomes the default workflow, while compile-time wrapper dependency instrumentation is preserved as an explicit opt-in mode. Current milestone: **P2.3 - First-Party Lint-Apply Driver**. A feasibility spike (`0b98e5f`) confirmed that `rustc_lint` suggestions can wrap function bodies and successfully reach `#[async_trait]` methods without regressing coverage. P2.4 follows with async dependency trampolines and R-4 resolution (`--extern` injection vs C-ABI) on the opt-in track.

## Documentation

- **[docs/phase2/](docs/phase2/)** - the Phase 2 implementation record, defect register, regression suite, and design plans for production hardening (unit identity, mirror isolation, policy gating, macro expansion, and graph scale). Start at [docs/phase2/README.md](docs/phase2/README.md); the architecture decisions taken during Phase 2 are recorded separately as [ADR-007 … ADR-012](docs/phase2/decision-records.md), continuing the Phase 0 numbering.
- **[docs/phase1/](docs/phase1/)** - the Phase 1 implementation record, architecture, empirical findings, and verification matrix for all milestones P1.1–P1.8 (wrapper interception, classification, AST analysis, surgical byte transformation, native OTel, async instrumentation, dependency trampolines, registry validation, and overhead benchmarks). Start at [docs/phase1/README.md](docs/phase1/README.md).
- **[docs/research/](docs/research/)** - the full Phase 0 investigation: landscape survey, architecture candidates, the [instrumentation semantics specification](docs/research/16-instrumentation-semantics.md) (the correctness oracle Phase 1's tests are written against), the [architecture decision records](docs/research/17-decision-records.md), and four rounds of verification (hands-on experiments, an adversarial review, a maintainer Q&A round, and a validated experiment matrix). Start at [docs/research/README.md](docs/research/README.md).
- Later phases receive their own sibling documentation folders under `docs/` as milestones land. `docs/research/` remains specifically the archived Phase 0 record and stays frozen.

## CLI Usage & Prototype Demonstration

`cargo-instrument` operates in two primary modes:
1. **Interactive CLI**: Standalone AST inspection (`analyze`) and surgical transformation preview (`transform`).
2. **Transparent Compiler Driver**: Invoking Cargo with `cargo run --bin cargo-instrument -- -- <cargo args...>` wraps `rustc` via `RUSTC_WRAPPER` and automatically routes build artifacts to an isolated directory (`target/instrumented`, per [ADR-004](docs/research/17-decision-records.md)).

### 1. Live End-to-End Application Telemetry (The Hero Flow)

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
