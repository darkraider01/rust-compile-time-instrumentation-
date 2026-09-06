<div align="center">

# rust-compile-time-instrumentation

**Zero-code compile-time OpenTelemetry instrumentation for Rust reaching third-party dependencies, on stable Rust.**

[![CI](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/workflows/ci.yml/badge.svg)](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/workflows/ci.yml)
[![Integration](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/workflows/integration.yml/badge.svg)](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/workflows/integration.yml)
[![Rust](https://img.shields.io/badge/rust-stable-orange?logo=rust)](https://www.rust-lang.org)
[![No nightly](https://img.shields.io/badge/nightly-not%20required-brightgreen)](docs/research/17-decision-records.md)
[![License: Apache 2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Phase](https://img.shields.io/badge/phase-1%20in%20progress-yellow)](#status)

</div>

Instruments Rust applications *and their dependencies* at build time - no source annotations, no manual span wiring - by intercepting `rustc` via `RUSTC_WRAPPER` and splicing native OpenTelemetry calls at the byte level. Runs on stable Rust with no compiler forks, no MIR passes, and no eBPF; the frozen architecture and the evidence behind it are in [`docs/research/`](docs/research/).

## Status

| Phase | Status | Focus |
| --- | --- | --- |
| **Phase 0 - Landscape Research & Architecture** | **Complete** (Frozen) | Six frozen architecture decisions ([ADR-001 … ADR-006](docs/research/17-decision-records.md)), normative correctness spec ([§16](docs/research/16-instrumentation-semantics.md)), experiment matrix ([Appendix E](docs/research/appendix-e-experiment-matrix.md)) |
| **Phase 1 - `cargo-instrument` Tool** | **In Progress** | Stable Rust compile-time instrumentation pipeline: P1.1–P1.6 complete (native async OpenTelemetry code generation verified); P1.7 is next |
| **Phase 2 - Production Hardening** | **Planned** | Workspace coverage, MSRV/toolchain compatibility, incremental compilation, large dependency graphs, cross-platform validation |
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
**Status:** IN PROGRESS

**Goal:** Build the compile-time instrumentation pipeline on stable Rust.

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
- [ ] **P1.7 - Dependency instrumentation / `extern "C"` trampolines** - NEXT
  Extend instrumentation to upstream Cargo dependencies using `extern "C"` ABI trampolines (`__otel_span_enter` / `__otel_span_exit`), resolved at final application link time.
- [ ] **P1.8 - End-to-end validation**
  Validate emitted spans against an OpenTelemetry collector, measure compile-time overhead, verify build-cache isolation, and test across sample multi-crate applications.

---

### Phase 2 - Production Hardening
**Status:** PLANNED

**Goal:** Establish the reliability, usability, and scale required for production build environments.

Planned areas:
- **Broader Cargo/workspace coverage:** Full support for complex virtual workspaces, custom build profiles, and Cargo features.
- **Toolchain & MSRV compatibility:** Formalize MSRV policy and test across supported stable compiler releases.
- **Performance optimization:** Minimize AST traversal and parsing overhead during incremental and full builds.
- **Incremental compilation:** Ensure tight integration with rustc's incremental cache without invalidating unchanged compilation units.
- **Cross-platform validation:** Comprehensive testing across Tier 1 platforms (Linux x86_64/aarch64, Windows, macOS).
- **Large dependency graphs:** Stress-testing on industrial dependency trees (e.g. 500+ crates).
- **OpenTelemetry Rust compatibility:** Track upcoming API/SDK changes in the `opentelemetry` crate ecosystem.

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

### Current Focus

**Phase 1 - P1.5: Native OpenTelemetry code generation**

Phase 0 established the architecture and invariants. Milestones P1.1–P1.4 have established and validated the compiler interception, multi-file source discovery, AST byte-span identification, and surgical byte-range transformation engine integrated into the compiler wrapper (verified with 78 automated tests across Linux, Windows, and macOS). The immediate next step is P1.5: implementing native OpenTelemetry API code generation to replace the minimal sentinel with runtime span lifecycle management.

## Documentation

- **[docs/phase1/](docs/phase1/)** - the Phase 1 implementation record, architecture, empirical findings, and verification matrix for milestones P1.1–P1.4 (Cargo/wrapper interception, classification, AST analysis, surgical byte transformation, and adversarial review resolutions). Start at [docs/phase1/README.md](docs/phase1/README.md).
- **[docs/research/](docs/research/)** - the full Phase 0 investigation: landscape survey, architecture candidates, the [instrumentation semantics specification](docs/research/16-instrumentation-semantics.md) (the correctness oracle Phase 1's tests are written against), the [architecture decision records](docs/research/17-decision-records.md), and four rounds of verification (hands-on experiments, an adversarial review, a maintainer Q&A round, and a validated experiment matrix). Start at [docs/research/README.md](docs/research/README.md).
- Later phases receive their own sibling documentation folders under `docs/` as milestones land. `docs/research/` remains specifically the archived Phase 0 record and stays frozen.

## The tool

`cargo-instrument/` - the Cargo subcommand and `RUSTC_WRAPPER` implementation.

```bash
cargo build --workspace
cargo test --workspace
cargo run --bin cargo-instrument -- analyze path/to/file.rs   # standalone AST/byte-span analysis
cargo run --bin cargo-instrument -- -- build                  # wrapped build, isolated target/instrumented (ADR-004)
```

CI runs `fmt`/`clippy`/`test`/`build` across Linux, Windows, and macOS ([`ci.yml`](.github/workflows/ci.yml)), plus a dedicated real-subprocess integration workflow ([`integration.yml`](.github/workflows/integration.yml)) that documents exactly which claims about the wrapper/Cargo integration are proven by the current test suite.

## License

[Apache License, Version 2.0](LICENSE).
