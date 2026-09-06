# Milestone P1.8: End-to-End Validation (Unseen Crates.io Dependency)

## Objective

Validate whether the existing `cargo-instrument` compile-time OpenTelemetry instrumentation architecture generalizes to genuinely **unseen real-world crates** from crates.io without crate-specific engineering, manual intervention, or production code modifications.

## Selected Target

- **Crate:** [`cesu8`](https://crates.io/crates/cesu8/1.1.0)
- **Version:** `1.1.0`
- **Domain:** CESU-8 / Java-CESU-8 string encoding and decoding
- **Status as Unseen:** 0 occurrences in repository codebase, fixtures, or tests prior to this experiment.

## Status & Final Verdict

| Metric | Status |
| :--- | :--- |
| **Experiment Status** | **Complete** |
| **Final Verdict** | **PASS** |
| **Production Code Changes** | **0 lines** |
| **Registry Cache Modifications** | **0 bytes (100% Bit-for-Bit Immutability)** |
| **AST Candidate Reconciliation** | **15 eligible + 3 skipped = 18 total (100% exact)** |
| **Runtime Dependency Spans** | **16 captured across 5 call depths** |
| **Leaked Handles** | **0 (`active_span_count() == 0`)** |

## Key Findings

1. **Automatic Interception & Mirroring:** `cargo-instrument` seamlessly intercepted `cesu8 v1.1.0` via `RUSTC_WRAPPER`, parsed its AST, discovered 15 candidate functions, and generated an isolated staging mirror in `target/instrumented/debug/deps/instrumented_sources/cesu8/`.
2. **Registry Source Immutability:** Cryptographic SHA-256 hashes of all 8 files in `~/.cargo/registry/src/.../cesu8-1.1.0/` verified that the user's Cargo cache remained untouched before and after compilation.
3. **Cross-Crate Context Propagation:** All 16 dependency spans captured by `InMemorySpanExporter` inherited the application's root `trace_id` (`01a3c2dde00640969315bdabade097ab`) and established a multi-level caller-callee hierarchy under `app_workflow`.
4. **Error Status Recording:** Spliced closure wrappers correctly detected `Result::Err(Cesu8DecodingError)` when invalid surrogates were parsed, setting `status=Error` on both `from_cesu8` and `from_cesu8_internal` spans while preserving `status=Unset` on successful calls.
5. **No Production Workarounds:** No changes were made to `cargo-instrument/src/*` or `otel-shim/src/*`.

## Evidence Directory

All raw execution outputs, cryptographic hashes, and telemetry logs are preserved in [`evidence/`](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/docs/phase1/p1.8/evidence/):

- [`baseline.txt`](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/docs/phase1/p1.8/evidence/baseline.txt): Uninstrumented execution output proving normal operation with 0 dependency spans.
- [`discovery.txt`](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/docs/phase1/p1.8/evidence/discovery.txt): Raw AST candidate discovery report and 100% reconciliation identity math.
- [`instrumentation.txt`](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/docs/phase1/p1.8/evidence/instrumentation.txt): Compiler interception logs showing automated AST splicing and mirror creation.
- [`build.txt`](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/docs/phase1/p1.8/evidence/build.txt): Cargo build and Clippy validation (`-D warnings` with 0 warnings).
- [`runtime-spans.txt`](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/docs/phase1/p1.8/evidence/runtime-spans.txt): Complete span dump with 17 spans, trace IDs, parent span IDs, and error statuses.
- [`source-fidelity.txt`](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/docs/phase1/p1.8/evidence/source-fidelity.txt): Pre-run vs. post-run SHA-256 cryptographic verification of Cargo cache files.

For the full 12-section validation report, see [**validation-report.md**](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/docs/phase1/p1.8/validation-report.md).

## Reproduction Instructions

To reproduce this experiment:

```powershell
# 1. Build cargo-instrument binary
cargo build --bin cargo-instrument

# 2. Run automated registry reconciliation test across all sample crates
$env:CARGO_INSTRUMENT_REGISTRY="1"
cargo test --test e2e_registry_tests test_dependency_coverage_and_reconciliation_table -- --ignored --nocapture

# 3. Analyze cesu8 directly via CLI
cargo run --bin cargo-instrument -- analyze "$((Resolve-Path "$env:USERPROFILE\.cargo\registry\src\index.crates.io-*\cesu8-1.1.0\src\lib.rs").Path)"
```
