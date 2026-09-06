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

3. **`urlencoding = "=2.1.3"`** (Second Unseen Real-World Target):
   - Domain: RFC 3986 URL percent-encoding and decoding with fallible UTF-8 validation.
   - Structure: Multi-file library (`src/lib.rs`, `src/enc.rs`, `src/dec.rs`, 11 distribution files).
   - Status as Unseen: 0 prior occurrences in repository codebase, fixtures, or tests.
   - Validated: Fallible `Result<Cow<str>, FromUtf8Error>` error status detection (`%FF%FF%FF` -> `Status::Error{""}`, reinforcing A7), 8 finished spans, 2-level intra-crate call parenting (`app` -> `append_string` -> `encode_into`), universal candidate reconciliation identity $5 + 21 = 26$ (A9), 100% bit-for-bit registry immutability (A11), and active span count 0 (A8).

4. **`async-trait = "0.1.89"`** (Macro Helper):
   - Validated: AST analysis and universal candidate reconciliation identity $34 + 21 = 55$ (A9).

## Acceptance Criteria Summary (A1–A18)

| Gate | Category | Description | Status | Evidence File |
| :--- | :--- | :--- | :---: | :--- |
| **A1** | Discovery | Registry crates classified correctly (`RegistryDependency`) | **PASS** | `discovery.txt`, `census-reconciliation.txt`, `urlencoding-discovery.txt` |
| **A2** | Discovery | `#![no_std]`, `forbid(unsafe_code)` skipped with logged reason | **PASS** | `discovery.txt`, `census-runtime-spans.txt`, `urlencoding-discovery.txt` |
| **A3** | Transformation | Registry crate mirrors preserved with module tree intact | **PASS** | `instrumentation.txt`, `source-fidelity.txt`, `urlencoding-instrumentation.txt` |
| **A4** | Linking | App + instrumented registry dep links and runs without linker error | **PASS** | `census-runtime-spans.txt`, `build.txt`, `urlencoding-build.txt` |
| **A5** | Telemetry | Spans from crates.io dependencies emitted with `SpanKind::Internal` | **PASS** | `census-runtime-spans.txt`, `runtime-spans.txt`, `urlencoding-runtime-spans.txt` |
| **A6** | Telemetry | Cross-crate parenting: dep span `parent_span_id` == app caller `span_id` | **PASS** | `census-runtime-spans.txt`, `runtime-spans.txt`, `urlencoding-runtime-spans.txt` |
| **A7** | Telemetry | Status handling: `Ok` -> `Status::Unset`, `Err` -> `Status::Error{""}` | **PASS** | `census-runtime-spans.txt` (`Ok`), `runtime-spans.txt` (`Err`), `urlencoding-runtime-spans.txt` (`Err`) |
| **A8** | Lifecycle | `otel_shim::active_span_count() == 0` after scenario completion | **PASS** | `census-runtime-spans.txt`, `runtime-spans.txt`, `urlencoding-runtime-spans.txt` |
| **A9** | Coverage | Universal reconciliation: $\text{candidates} + \sum \text{skipped} = \text{total\_fns}$ | **PASS** | `census-reconciliation.txt`, `discovery.txt`, `urlencoding-discovery.txt` |
| **A10** | Compatibility | Heavy crates (`tokio`, etc.) build uninstrumented via fail-open | **PASS** | `discovery.txt`, `trampoline_tests.rs` |
| **A11** | Source Fidelity | Original registry sources 100% bit-for-bit unchanged (SHA-256 tree match) | **PASS** | `census-source-fidelity.txt`, `source-fidelity.txt`, `urlencoding-source-fidelity.txt` |
| **A12** | Isolation | Zero `.rs` files modified or created outside `target/instrumented/**` | **PASS** | `census-source-fidelity.txt`, `source-fidelity.txt`, `urlencoding-source-fidelity.txt` |
| **A13** | Correctness | Cargo 5-pass correctness: clean, repeat, incremental app, incremental dep | **PASS** | `benchmark.txt`, `cargo_integration_tests.rs` |
| **A14** | Correctness | `Cargo.toml` and `Cargo.lock` unchanged | **PASS** | `source-fidelity.txt`, `cargo_integration_tests.rs` |
| **A15** | Safety | Malformed/truncated source fails open cleanly without panic | **PASS** | `trampoline_tests.rs` (`test_malformed_syntax_fail_open_s11`) |
| **A16** | Safety | 7 C-ABI symbol collisions and escaping `#[path]` sandbox safe | **PASS** | `trampoline_tests.rs` (`test_abi_symbol_collision_fail_open_s11`) |
| **A17** | Performance | Compile-time, runtime, and binary size overhead measured and recorded | **PASS** | `benchmark.txt` (`bench_overhead.rs`) |
| **A18** | Reproducibility| Mirrored sources 100% byte-reproducible across consecutive passes | **PASS** | `e2e_registry_tests.rs` (`test_mirror_byte_reproducibility`) |

## Complete Raw Evidence Index

All raw execution outputs, cryptographic hashes, and telemetry logs are preserved in [`evidence/`](evidence/):

- [`environment.txt`](evidence/environment.txt): Exact compiler (`rustc 1.97.1`), Cargo (`cargo 1.97.1`), OS, and CPU architecture.
- [`dependency-identity.txt`](evidence/dependency-identity.txt): Pinned crate versions, registry paths, and metadata for `census-0.4.2`, `cesu8-1.1.0`, `urlencoding-2.1.3`, and `async-trait-0.1.89`.
- [`census-runtime-spans.txt`](evidence/census-runtime-spans.txt): 26 runtime spans captured across crate boundary from `census = "=0.4.2"`.
- [`census-reconciliation.txt`](evidence/census-reconciliation.txt): Exact AST reconciliation output ($16+16=32$ on `census`, $34+21=55$ on `async-trait`).
- [`census-source-fidelity.txt`](evidence/census-source-fidelity.txt): Cryptographic SHA-256 tree of all 14 files in `census-0.4.2` before and after build.
- [`baseline.txt`](evidence/baseline.txt): Uninstrumented execution output for `cesu8` proving 0 dependency spans by default.
- [`discovery.txt`](evidence/discovery.txt): Raw AST candidate discovery report and exact reconciliation ($15+3=18$) on `cesu8`.
- [`instrumentation.txt`](evidence/instrumentation.txt): Interception logs showing automated AST splicing and mirror creation for `cesu8`.
- [`build.txt`](evidence/build.txt): Cargo build and Clippy validation (`-D warnings` with 0 warnings) for `cesu8`.
- [`runtime-spans.txt`](evidence/runtime-spans.txt): 17 spans, trace propagation, 5-level call tree, and `Status::Error{""}` detection for `cesu8`.
- [`source-fidelity.txt`](evidence/source-fidelity.txt): Cryptographic SHA-256 tree of all 8 files in `cesu8-1.1.0` before and after build.
- [`urlencoding-baseline.txt`](evidence/urlencoding-baseline.txt): Uninstrumented execution output for `urlencoding-2.1.3` proving 0 dependency spans by default.
- [`urlencoding-discovery.txt`](evidence/urlencoding-discovery.txt): Raw AST candidate discovery and reconciliation ($5+21=26$) on `urlencoding-2.1.3`.
- [`urlencoding-instrumentation.txt`](evidence/urlencoding-instrumentation.txt): Interception logs and mirror staging for multi-module `urlencoding-2.1.3`.
- [`urlencoding-build.txt`](evidence/urlencoding-build.txt): Cargo build output for instrumented `urlencoding-2.1.3` app.
- [`urlencoding-runtime-spans.txt`](evidence/urlencoding-runtime-spans.txt): 8 spans, context propagation, intra-crate parenting, and `Status::Error{""}` for `urlencoding-2.1.3`.
- [`urlencoding-source-fidelity.txt`](evidence/urlencoding-source-fidelity.txt): Cryptographic SHA-256 tree of all 11 files in `urlencoding-2.1.3` proving 0 bytes modified.
- [`benchmark.txt`](evidence/benchmark.txt): Empirical compile-time ($N=5$), runtime ($M=100,000$), and binary size overhead measurements.

For the dedicated 12-section validation report on unseen crate `cesu8`, see [**validation-report.md**](validation-report.md).

## Automated Test Suite Verification (143 Tests across 10 Suites)

The test suite enforces explicit opt-in gating for tests requiring network or local cargo registry caches. In the default offline run (`cargo test --workspace`), the 4 registry integration tests are **visibly ignored** (not silently skipped or masked) per M2 safety guarantees.

| Test Suite Target | Default Run (`cargo test --workspace`) | Gated Run (`CARGO_INSTRUMENT_REGISTRY=1 ... --include-ignored`) | Scope / Category |
| :--- | :---: | :---: | :--- |
| `tests/ast_tests.rs` | 34 passed | 34 passed | AST discovery, normalization, trait methods, exclusions, syntax parsing |
| `tests/byte_span_tests.rs` | 4 passed | 4 passed | Exact UTF-8 byte span offsets, unicode, multibyte, formatting |
| `tests/cargo_integration_tests.rs` | 7 passed | 7 passed | Real Cargo subprocesses, 5-pass correctness, isolation, CLI analyze |
| `tests/discovery_tests.rs` | 11 passed | 11 passed | Compiler invocation classification, argument parsing, crate roles |
| `tests/e2e_registry_tests.rs` | **3 ignored** | **3 passed** | Gated real-registry E2E tests (`census-0.4.2` telemetry, candidate table, mirror reproducibility) |
| `tests/native_otel_tests.rs` | 24 passed | 24 passed | Native OTel sync & async codegen, 16-point async matrix, in-memory exporter proofs |
| `tests/trampoline_tests.rs` | 13 passed, **1 ignored** | **14 passed** | Tier 2 C-ABI trampolines (includes 1 gated test: `test_registry_source_cache_immutability`) |
| `tests/transform_tests.rs` | 35 passed | 35 passed | Surgical byte splicing, comments/formatting preservation, CLI transform |
| `tests/wrapper_tests.rs` | 5 passed | 5 passed | `RUSTC_WRAPPER` argument forwarding, exit code propagation, recursion guards |
| `otel-shim/src/lib.rs` | 6 passed | 6 passed | Standalone runtime shim C-ABI invariants: LIFO context stack, handle safety |
| **Total** | **139 passed, 4 ignored** | **143 passed, 0 ignored** | **143 total tests across workspace (100% pass rate)** |

### Visibly Ignored Verification (Default Run)
When running `cargo test --workspace`, the 4 registry tests output explicit ignore messages:
- `test test_dependency_coverage_and_reconciliation_table ... ignored, requires CARGO_INSTRUMENT_REGISTRY=1 and cached registry crates`
- `test test_e2e_census_runtime_telemetry ... ignored, requires CARGO_INSTRUMENT_REGISTRY=1 and cached census-0.4.2`
- `test test_mirror_byte_reproducibility ... ignored, requires CARGO_INSTRUMENT_REGISTRY=1 and cached registry crates`
- `test test_registry_source_cache_immutability ... ignored, requires CARGO_INSTRUMENT_REGISTRY=1 and cached census-0.4.2`

## Reproduction Instructions

```powershell
# 1. Run 139 default offline unit & integration tests (4 registry tests visibly ignored)
cargo test --workspace

# 2. Run the 4 gated real-registry E2E integration tests only
$env:CARGO_INSTRUMENT_REGISTRY="1"
cargo test --workspace -- --ignored --nocapture

# 3. Run all 143 tests in a single command (including gated registry tests)
$env:CARGO_INSTRUMENT_REGISTRY="1"
cargo test --workspace -- --include-ignored

# 4. Run overhead benchmark suite (A17)
cargo bench --bench bench_overhead

# 5. Demonstrate live dependency telemetry via demo_app (census-0.4.2)
cargo run --bin cargo-instrument -- -- run --manifest-path examples/demo_app/Cargo.toml
```
