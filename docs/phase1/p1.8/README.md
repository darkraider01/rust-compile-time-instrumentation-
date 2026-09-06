# Milestone P1.8: End-to-End Validation (Registry Crate Telemetry & Overhead Benchmarks)

## Objective

Validate whether the `cargo-instrument` compile-time OpenTelemetry instrumentation pipeline reliably generalizes to real external dependencies resolved from crates.io without modifying their original source files, correctly links and propagates distributed trace context at runtime, proves mathematical candidate reconciliation, preserves bit-for-bit registry immutability, and operates within acceptable build and runtime performance budgets.

## Target Dependencies Evaluated

1. **`census = "=0.4.2"`** (Primary Integration Baseline):
   - Domain: In-memory object tracking and lifecycle accounting.
   - Structure: Single-file crate (`src/lib.rs`, 32 functions).
   - Validated: C-ABI trampoline linking (A4), `SpanKind::Internal` emission (A5), cross-crate parenting under caller app span (A6), success status `Unset` (A7 partial), zero leaked handles (A8), universal candidate reconciliation identity $16 + 16 = 32$ (A9), registry source cache immutability (A11), and mirror byte reproducibility across passes (A18).

2. **`cesu8 = "=1.1.0"`** (Unseen Real-World Target):
   - Domain: CESU-8 string encoding and decoding with fallible conversions.
   - Structure: Multi-file library (`src/lib.rs` and `src/unicode.rs`, 18 functions).
   - Status as Unseen: 0 prior occurrences in repository codebase, fixtures, or tests.
   - Validated: Fallible `Result<Cow<str>, Cesu8DecodingError>` error status detection (`Err` -> `Status::Error{""}`, completing A7), 16 spans across 5 call depths, multi-module staging mirror preservation, and zero production code modifications.

3. **`async-trait = "0.1.89"`** (Macro Helper):
   - Validated: AST analysis and universal candidate reconciliation identity $34 + 21 = 55$ (A9).

## Acceptance Criteria Summary (A1–A18)

| Gate | Category | Description | Status | Evidence File |
| :--- | :--- | :--- | :---: | :--- |
| **A1** | Discovery | Registry crates classified correctly (`RegistryDependency`) | **PASS** | `discovery.txt`, `census-reconciliation.txt` |
| **A2** | Discovery | `#![no_std]`, `forbid(unsafe_code)` skipped with logged reason | **PASS** | `discovery.txt`, `census-runtime-spans.txt` |
| **A3** | Transformation | Registry crate mirrors preserved with module tree intact | **PASS** | `instrumentation.txt`, `source-fidelity.txt` |
| **A4** | Linking | App + instrumented registry dep links and runs without linker error | **PASS** | `census-runtime-spans.txt`, `build.txt` |
| **A5** | Telemetry | Spans from crates.io dependencies emitted with `SpanKind::Internal` | **PASS** | `census-runtime-spans.txt`, `runtime-spans.txt` |
| **A6** | Telemetry | Cross-crate parenting: dep span `parent_span_id` == app caller `span_id` | **PASS** | `census-runtime-spans.txt`, `runtime-spans.txt` |
| **A7** | Telemetry | Status handling: `Ok` -> `Status::Unset`, `Err` -> `Status::Error{""}` | **PASS** | `census-runtime-spans.txt` (`Ok`), `runtime-spans.txt` (`Err`) |
| **A8** | Lifecycle | `otel_shim::active_span_count() == 0` after scenario completion | **PASS** | `census-runtime-spans.txt`, `runtime-spans.txt` |
| **A9** | Coverage | Universal reconciliation: $\text{candidates} + \sum \text{skipped} = \text{total\_fns}$ | **PASS** | `census-reconciliation.txt`, `discovery.txt` |
| **A10** | Compatibility | Heavy crates (`tokio`, etc.) build uninstrumented via fail-open | **PASS** | `discovery.txt`, `trampoline_tests.rs` |
| **A11** | Source Fidelity | Original registry sources 100% bit-for-bit unchanged (SHA-256 tree match) | **PASS** | `census-source-fidelity.txt`, `source-fidelity.txt` |
| **A12** | Isolation | Zero `.rs` files modified or created outside `target/instrumented/**` | **PASS** | `census-source-fidelity.txt`, `source-fidelity.txt` |
| **A13** | Correctness | Cargo 5-pass correctness: clean, repeat, incremental app, incremental dep | **PASS** | `benchmark.txt`, `cargo_integration_tests.rs` |
| **A14** | Correctness | `Cargo.toml` and `Cargo.lock` unchanged | **PASS** | `source-fidelity.txt`, `cargo_integration_tests.rs` |
| **A15** | Safety | Malformed/truncated source fails open cleanly without panic | **PASS** | `trampoline_tests.rs` (`test_malformed_syntax_fail_open_s11`) |
| **A16** | Safety | 7 C-ABI symbol collisions and escaping `#[path]` sandbox safe | **PASS** | `trampoline_tests.rs` (`test_abi_symbol_collision_fail_open_s11`) |
| **A17** | Performance | Compile-time, runtime, and binary size overhead measured and recorded | **PASS** | `benchmark.txt` (`bench_overhead.rs`) |
| **A18** | Reproducibility| Mirrored sources 100% byte-reproducible across consecutive passes | **PASS** | `e2e_registry_tests.rs` (`test_mirror_byte_reproducibility`) |

## Complete Raw Evidence Index

All raw execution outputs, cryptographic hashes, and telemetry logs are preserved in [`evidence/`](evidence/):

- [`environment.txt`](evidence/environment.txt): Exact compiler (`rustc 1.97.1`), Cargo (`cargo 1.97.1`), OS, and CPU architecture.
- [`dependency-identity.txt`](evidence/dependency-identity.txt): Pinned crate versions, registry paths, and metadata for `census-0.4.2`, `cesu8-1.1.0`, and `async-trait-0.1.89`.
- [`census-runtime-spans.txt`](evidence/census-runtime-spans.txt): 26 runtime spans captured across crate boundary from `census = "=0.4.2"`.
- [`census-reconciliation.txt`](evidence/census-reconciliation.txt): Exact AST reconciliation output ($16+16=32$ on `census`, $34+21=55$ on `async-trait`).
- [`census-source-fidelity.txt`](evidence/census-source-fidelity.txt): Cryptographic SHA-256 tree of all 14 files in `census-0.4.2` before and after build.
- [`baseline.txt`](evidence/baseline.txt): Uninstrumented execution output for `cesu8` proving 0 dependency spans by default.
- [`discovery.txt`](evidence/discovery.txt): Raw AST candidate discovery report and exact reconciliation ($15+3=18$) on `cesu8`.
- [`instrumentation.txt`](evidence/instrumentation.txt): Interception logs showing automated AST splicing and mirror creation for `cesu8`.
- [`build.txt`](evidence/build.txt): Cargo build and Clippy validation (`-D warnings` with 0 warnings) for `cesu8`.
- [`runtime-spans.txt`](evidence/runtime-spans.txt): 17 spans, trace propagation, 5-level call tree, and `Status::Error{""}` detection for `cesu8`.
- [`source-fidelity.txt`](evidence/source-fidelity.txt): Cryptographic SHA-256 tree of all 8 files in `cesu8-1.1.0` before and after build.
- [`benchmark.txt`](evidence/benchmark.txt): Empirical compile-time ($N=5$), runtime ($M=100,000$), and binary size overhead measurements.

For the dedicated 12-section validation report on unseen crate `cesu8`, see [**validation-report.md**](validation-report.md).

## Reproduction Instructions

```powershell
# 1. Run 139 default offline unit & integration tests
cargo test --workspace

# 2. Run 4 gated real-registry E2E integration tests (honest opt-in via --ignored)
$env:CARGO_INSTRUMENT_REGISTRY="1"
cargo test --workspace -- --ignored --nocapture

# 3. Run overhead benchmark suite (A17)
cargo bench --bench bench_overhead

# 4. Demonstrate live dependency telemetry via demo_app (census-0.4.2)
cargo run --bin cargo-instrument -- -- run --manifest-path examples/demo_app/Cargo.toml
```
