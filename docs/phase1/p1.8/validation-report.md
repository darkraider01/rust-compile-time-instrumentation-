# P1.8 End-to-End Validation: Unseen Crates.io Dependency

## 1. Target

- **Crate:** `cesu8`
- **Version:** `1.1.0`
- **Source:** [crates.io/crates/cesu8/1.1.0](https://crates.io/crates/cesu8/1.1.0)
- **Selection Rationale:**
  - Real, widely used data transformation and encoding/decoding library published on crates.io.
  - Exposes functions returning `Result<Cow<str>, Cesu8DecodingError>` (`from_cesu8`, `from_java_cesu8`) enabling validation of error status detection (`__otel_span_set_error`).
  - Defines custom error types implementing `std::error::Error` and `std::fmt::Display`.
  - Exercises standard library types and lifetimes (`Cow<'a, str>`, `&'a [u8]`) without `#![no_std]` or `#![forbid(unsafe_code)]` exclusions.
  - Zero external dependencies, ensuring that all compiler and linker observations directly reflect `cesu8` and the instrumentation wrapper.
- **Unseen Qualification:**
  - `cesu8` was never included in any existing test, fixture, benchmark, example, or documentation in this repository.
  - A full-repository adversarial grep for `cesu8` across all Rust source files, tests, and documentation returned 0 results prior to this experiment.

## 2. Environment

- **OS:** Microsoft Windows NT 10.0.26200.0 (Windows 11)
- **Architecture:** x86_64 (X64, `x86_64-pc-windows-msvc`)
- **Rust Toolchain:** `rustc 1.97.1 (8bab26f4f 2026-07-14)`
- **Cargo Version:** `cargo 1.97.1 (c980f4866 2026-06-30)`
- **Relevant Dependencies:**
  - `cesu8 = "=1.1.0"`
  - `opentelemetry = "0.32.0"`
  - `opentelemetry_sdk = "0.32.1"`
  - `otel-shim = { path = ".../otel-shim" }`

## 3. Baseline

- **Objective:** Verify that `cesu8` and the test application execute normally without instrumentation, and establish that no dependency spans are emitted by default.
- **Command:**
  ```powershell
  cargo run --manifest-path "C:\Users\branybuck\AppData\Local\Temp\p1_8_baseline_f27d3ba5-bc64-407b-8e19-ac7c45e509ad\Cargo.toml"
  ```
- **Build Result:**
  ```text
  Compiling cesu8 v1.1.0
  Compiling cesu8_validation_app v0.1.0
  Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.72s
  ```
- **Runtime Result:**
  ```text
  BASELINE_START
  to_cesu8 result len: 20
  from_cesu8 success: Hello, world! 🚀
  from_cesu8 expected error: could not convert CESU-8 data to UTF-8
  is_valid_cesu8: false
  FINISHED_SPANS_COUNT=1
  SPAN: name=application_root kind=Internal parent=0000000000000000
  BASELINE_SUCCESS
  ```
- **Finding:** Normal execution produced 1 span (the application's own root span). Exactly 0 dependency spans were recorded.

## 4. Instrumented Execution

- **Command:**
  ```powershell
  $env:CARGO_INSTRUMENT_REGISTRY = "1"
  cargo-instrument build --manifest-path "C:\Users\branybuck\AppData\Local\Temp\p1_8_baseline_f27d3ba5-bc64-407b-8e19-ac7c45e509ad\Cargo.toml"
  ```
- **Configuration:**
  - `CARGO_INSTRUMENT_REGISTRY=1`: Enables third-party registry dependency interception (§12.1, §12.3).
  - Isolated target directory: `target/instrumented` (ADR-004).
  - Staging directory: `target/instrumented/debug/deps/instrumented_sources/cesu8`.
- **Discovery Output Summary:**
  - 18 total functions discovered in `cesu8/src/lib.rs`.
  - 15 eligible candidate functions discovered.
  - 3 skipped functions (1 `#[inline]`, 2 `#[cfg(test)]`).
- **Transformation Summary:**
  - 15 candidate functions transformed via `TrampolineEmitter` with C-ABI `extern "C"` declarations and RAII drop guards.
  - Mirrored root file `src/lib.rs` created in isolated staging mirror (31,641 bytes vs 16,846 bytes original).

## 5. Candidate Discovery & Exact AST Reconciliation

The discovery report for `cesu8/src/lib.rs` reconciled with mathematical precision:

$$\text{Eligible Candidates } (15) + \sum \text{Skipped Counters } (3) = \text{Total Functions } (18)$$

| Category | Count | Functions / Items |
| :--- | :---: | :--- |
| **Eligible Candidates** | **15** | `<Cesu8DecodingError as Error>::description`, `<Cesu8DecodingError as Error>::cause`, `<Cesu8DecodingError as Display>::fmt`, `from_cesu8`, `from_java_cesu8`, `from_cesu8_internal`, `decode_from_iter`, `dec_surrogate`, `dec_surrogates`, `to_cesu8`, `to_java_cesu8`, `to_cesu8_internal`, `is_valid_cesu8`, `is_valid_java_cesu8`, `enc_surrogate` |
| `inline_attribute` | 1 | `enc_surrogates` (annotated `#[inline]`) |
| `cfg_test` | 2 | `test_from_cesu8`, `test_to_cesu8` (inside `#[cfg(test)] mod tests`) |
| `adapter_trait` | 0 | None |
| `drop_implementation` | 0 | None |
| `const_fn` | 0 | None |
| `extern_abi` | 0 | None |
| `self_recursive` | 0 | None |
| `handwritten_otel` | 0 | None |
| `nested_function` | 0 | None |
| **Total Accounted** | **18** | **Exact 100% Reconciliation Identity Match** |

## 6. Source Fidelity & Registry Immutability

Before and after the entire compilation and execution pipeline, cryptographic SHA-256 hashes were calculated across all 8 files in `~/.cargo/registry/src/.../cesu8-1.1.0/`:

| File | Length | Pre-Run SHA-256 | Post-Run SHA-256 | Match Status |
| :--- | :---: | :--- | :--- | :---: |
| `.cargo-ok` | 7 B | `AFBF9D0F3560B0FD7795E81C42A0A79EE6B6FC67E064F77826AEE642CAD28D91` | `AFBF9D0F3560B0FD7795E81C42A0A79EE6B6FC67E064F77826AEE642CAD28D91` | **MATCH** |
| `.gitignore` | 20 B | `C1E953EE360E77DE57F7B02F1B7880BD6A3DC22D1A69E953C2AC2C52CC52D247` | `C1E953EE360E77DE57F7B02F1B7880BD6A3DC22D1A69E953C2AC2C52CC52D247` | **MATCH** |
| `.travis.yml` | 528 B | `54B629F18A31FC4DFA0BAEC0177ACC8627E5420A5E0C223AF39D2A57DDAFD7FB` | `54B629F18A31FC4DFA0BAEC0177ACC8627E5420A5E0C223AF39D2A57DDAFD7FB` | **MATCH** |
| `Cargo.toml` | 505 B | `78E66DD24C12E0AC858E7524CDD7D51D1EEE753C1E9C32E5B915DC9F1D767870` | `78E66DD24C12E0AC858E7524CDD7D51D1EEE753C1E9C32E5B915DC9F1D767870` | **MATCH** |
| `COPYRIGHT-RUST.txt`| 17,426 B | `5CA77347E58205D3B543C04A9C5BDD11D20A9F3108A7B246640EDFFA999B5F35` | `5CA77347E58205D3B543C04A9C5BDD11D20A9F3108A7B246640EDFFA999B5F35` | **MATCH** |
| `README.md` | 1,475 B | `4FCC5D9B5DB444AD3C33C79AEBE18DE4FA6351982646DA0B78830FBAD67F9E8F` | `4FCC5D9B5DB444AD3C33C79AEBE18DE4FA6351982646DA0B78830FBAD67F9E8F` | **MATCH** |
| `src/lib.rs` | 16,846 B | `402F647C80CCAA86F43A5571CC081AA66777D2A538CE9D9DC52A7E588EF137C9` | `402F647C80CCAA86F43A5571CC081AA66777D2A538CE9D9DC52A7E588EF137C9` | **MATCH** |
| `src/unicode.rs` | 1,381 B | `66CC902B4DD323CEF600890B6CD99186592919B03E55E3535ADF0C5B95A8CE45` | `66CC902B4DD323CEF600890B6CD99186592919B03E55E3535ADF0C5B95A8CE45` | **MATCH** |

**Result:** Zero registry files were modified or touched in place. All transformations took place strictly in `target/instrumented/debug/deps/instrumented_sources/cesu8/`.

## 7. Compilation Validation

- **Command:**
  ```powershell
  cargo-instrument build --manifest-path "<path>/Cargo.toml"
  cargo clippy --manifest-path "<path>/Cargo.toml" -- -D warnings
  ```
- **Results:**
  - `cargo-instrument build`: Clean exit code 0.
  - `cargo clippy -- -D warnings`: Clean exit code 0.
  - Transformed dependency source in staging compiled with `rustc` into `libcesu8-42ed0643b50f664b.rlib`.
  - Application crate linked cleanly with `otel-shim` and `cesu8` into `target/instrumented/debug/cesu8_validation_app.exe`.
  - 0 compilation errors, 0 warnings under `-D warnings`.

## 8. Runtime Telemetry Verification

- **Command:**
  ```powershell
  & "target\instrumented\debug\cesu8_validation_app.exe"
  ```
- **Exported Span Count:** 17 spans total.
  - 1 Application caller root span: `app_workflow` (`span_id=f0380b914811032e`).
  - 16 Dependency spans emitted dynamically from `cesu8`.
- **Span Kinds:** All 16 dependency spans were emitted with `SpanKind::Internal`.
- **Trace Context Propagation:**
  - Application root span: `trace_id=01a3c2dde00640969315bdabade097ab`.
  - Every single one of the 16 dependency spans carried `trace_id=01a3c2dde00640969315bdabade097ab` without loss.
- **Parent/Child Hierarchy:**
  - `app_workflow` (`f0380b914811032e`)
    - `to_cesu8` (`7fed27fdf0aa795d`, parent = `f0380b914811032e`)
      - `is_valid_cesu8` (`0521f29da15573bf`, parent = `7fed27fdf0aa795d`)
      - `to_cesu8_internal` (`159fd69f37621c06`, parent = `7fed27fdf0aa795d`)
        - `enc_surrogate` (`359cd87a96daf1bc`, parent = `159fd69f37621c06`)
        - `enc_surrogate` (`c63ab1d0621bc4aa`, parent = `159fd69f37621c06`)
    - `from_cesu8` [Ok] (`45ece4eb17b2e3ec`, parent = `f0380b914811032e`)
      - `from_cesu8_internal` (`879e93d7c1669e3a`, parent = `45ece4eb17b2e3ec`)
        - `decode_from_iter` (`a299a2399a1bcf83`, parent = `879e93d7c1669e3a`)
          - `dec_surrogates` (`b4e45c0809055b78`, parent = `a299a2399a1bcf83`)
            - `dec_surrogate` (`daf530f6d34ffd85`, parent = `b4e45c0809055b78`)
            - `dec_surrogate` (`580036588ecae5d8`, parent = `b4e45c0809055b78`)
    - `from_cesu8` [Err] (`c43173df35692096`, parent = `f0380b914811032e`, status = `Error`)
      - `from_cesu8_internal` (`a3edc68f8194ac61`, parent = `c43173df35692096`, status = `Error`)
        - `decode_from_iter` (`2f62d6b80a3c9d91`, parent = `a3edc68f8194ac61`, status = `Unset`)
    - `<Cesu8DecodingError as fmt::Display>::fmt` (`724b5cd6dc41f9be`, parent = `f0380b914811032e`)
    - `is_valid_cesu8` (`669b19249d255c49`, parent = `f0380b914811032e`)
- **Result Status:**
  - Successful calls recorded `status=Unset`.
  - The invalid surrogate call (`from_cesu8(&[0xED, 0xA0, 0x80])`) properly returned `Err(Cesu8DecodingError)`, triggering `__otel_span_set_error`, which marked both `from_cesu8` and `from_cesu8_internal` as `status=Error { description: "" }`.
- **Lifecycle & Memory Safety:**
  - `otel_shim::active_span_count()` returned 0 immediately after workflow execution.
  - Zero leaked handles or dangling thread-local frames.

## 9. Adversarial Crate-Specific Checks

- **Repo Grep for `cesu8`:** 0 matches in production code (`cargo-instrument/src`, `otel-shim/src`).
- **Special-case Heuristics:** Zero crate-specific rules or workarounds were added.
- **Preflight Requirement:** Application crate was required by general preflight validation to reference `otel_shim::init()` per ADR-003 / E-10 to prevent rustc dead-code pruning; this is a universal requirement for all application crates instrumenting dependencies and not specific to `cesu8`.

## 10. P1.8 Acceptance Matrix

| Property | Result | Evidence Reference |
| :--- | :---: | :--- |
| **Unseen crate** | **PASS** | `cesu8` v1.1.0 independently selected; 0 prior occurrences in repo |
| **Automatic discovery** | **PASS** | 15 candidates discovered automatically via `AstVisitor` (`discovery.txt`) |
| **Exact reconciliation** | **PASS** | 15 eligible + 3 skipped = 18 total functions (`discovery.txt`) |
| **Automatic transformation** | **PASS** | 15 trampolines generated and spliced into isolated mirror (`instrumentation.txt`) |
| **Registry immutability** | **PASS** | 8 of 8 original Cargo registry files bit-for-bit unchanged (`source-fidelity.txt`) |
| **Dependency compilation** | **PASS** | Mirrored `cesu8` compiled cleanly with rustc into rlib (`build.txt`) |
| **Linking** | **PASS** | Application binary linked with `libotel_shim.rlib` and `libcesu8.rlib` (`build.txt`) |
| **Runtime execution** | **PASS** | Executed with exit code 0 (`runtime-spans.txt`) |
| **Real OpenTelemetry spans** | **PASS** | 16 dynamic dependency spans captured by `InMemorySpanExporter` (`runtime-spans.txt`) |
| **Cross-crate parenting** | **PASS** | All 16 dependency spans attached under root `trace_id` and caller `span_id` (`runtime-spans.txt`) |
| **Result error status** | **PASS** | `from_cesu8` on invalid bytes recorded `status=Error` (`runtime-spans.txt`) |
| **Lifecycle / handle balance** | **PASS** | `active_span_count() == 0` (0 leaked handles) (`runtime-spans.txt`) |
| **No crate-specific workaround** | **PASS** | 0 production code changes; zero crate-specific symbols in implementation |
| **Async behavior** | **NOT EXERCISED** | `cesu8` contains synchronous functions only |
| **Cancellation** | **NOT EXERCISED** | Synchronous execution; async cancellation not applicable |

## 11. Limitations

1. **`#![no_std]` Crates:** C-ABI trampoline instrumentation intentionally skips `#![no_std]` crates per §12.3 because `otel-shim` requires `std` (for thread-local context management and OpenTelemetry SDK integration). Crates with `#![no_std]` (such as `adler2`, `arrayvec`, `byteorder`) are discovered and reconciled by the AST engine, but will fail-open (skip trampoline emission) during compilation.
2. **Synchronous Crate:** `cesu8` is a synchronous parser/codec library. While synchronous C-ABI trampolines, closure wrapping, Result error recording, and cross-crate parenting were completely proven, async runtime propagation across `.await` points was verified in Milestone P1.6/P1.7 tests (`native_otel_tests.rs`, `trampoline_tests.rs`) but was not exercised by `cesu8`.
3. **No Claim of Universal Compatibility:** This experiment proves that the architecture generalizes cleanly to an unseen, pure-Rust, `std`-compatible library without crate-specific engineering. It does not claim that all ~160,000 crates on crates.io will compile without encountering edge cases (such as complex macro-generated functions or conflicting C symbol names).

## 12. Final Verdict

### **PASS**

The existing `cargo-instrument` architecture successfully intercepted, analyzed, transformed, compiled, linked, and collected real OpenTelemetry spans from a genuinely unseen crates.io dependency (`cesu8 = "=1.1.0"`), with 100% exact AST reconciliation ($15+3=18$), complete bit-for-bit registry immutability across all 8 files, zero production code modifications, and 0 leaked span handles.
