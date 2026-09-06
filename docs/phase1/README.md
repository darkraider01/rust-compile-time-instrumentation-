← [Project overview](../../README.md) | [Phase 0 Research](../research/README.md)

---

# Phase 1 - Compile-Time Instrumentation Tool (`cargo-instrument`)

**Milestones covered:** P1.1, P1.2, P1.3, P1.4, P1.5, P1.6, P1.7  
**Status:** P1.1–P1.7 Complete; P1.8 Next  
**Toolchain:** Stable Rust (CI tests against latest `stable`; verified locally on 1.97.1; unpinned MSRV, formal policy deferred to Phase 2)  
**Core dependencies:** `syn` 2.0, `proc-macro2` 1.0, `quote` 1.0, `thiserror` 1.0  
**Test suite status:** 122 automated tests passing across Linux, Windows, and macOS (0 failures, 0 clippy warnings)  

---

## Phase Progression

```text
Phase 0 - Research & Architecture (Frozen)
          │
          ▼
Phase 1 - Compile-Time Instrumentation (In Progress - current focus)
          │
          ├── P1.1 RUSTC_WRAPPER Interception       ✅ Complete
          ├── P1.2 Source Discovery & Classification ✅ Complete
          ├── P1.3 syn AST + Exact Byte Spans       ✅ Complete
          ├── P1.4 Surgical Source Transformation   ✅ Complete
          ├── P1.5 Native OTel Code Generation      ✅ Complete
          ├── P1.6 Async Instrumentation            ✅ Complete
          ├── P1.7 Dependency Trampolines           ✅ Complete
          └── P1.8 End-to-End Validation            → NEXT
          │
          ▼
Phase 2 - Production Hardening (Planned)
          │
          ▼
Phase 3 - Evaluation & Research (Planned)
```

---

## 1. Executive Summary & Boundaries

Phase 1 translates the frozen architecture from [Phase 0](../research/README.md) into a working, stable-Rust compile-time instrumentation CLI and wrapper tool (`cargo-instrument`).

This document records the design decisions, implementation architecture, empirical findings, and verification proofs for the Phase 1 milestones:
- **P1.1 - Cargo / `RUSTC_WRAPPER` interception**
- **P1.2 - Source discovery & compilation-unit classification**
- **P1.3 - `syn` AST & exact byte-span analysis**
- **P1.4 - Surgical byte-range source transformation**
- **P1.5 - Native OpenTelemetry synchronous code generation**

### Invariant Boundaries for P1.1–P1.3
Per Phase 0 normative specifications ([§16](../research/16-instrumentation-semantics.md)):
1. **Analysis-only boundary:** P1.1–P1.3 perform observation and candidate discovery only. Zero source code rewriting, zero code generation, and zero OTLP/runtime wiring are performed in these milestones.
2. **Byte-for-byte source preservation (S1/S2):** Every source file inspected remains bit-for-bit identical before and after tool execution (verified by SHA-256 pre/post snapshots).
3. **Fail-open resilience (S11):** Any failure in discovery, parsing, or analysis logs a warning/debug message and permits `rustc` to compile the original crate unhindered.
4. **Target directory cache isolation (ADR-004):** Instrumented builds target `target/instrumented` to prevent clobbering Cargo's uninstrumented build cache.

---

## 2. Milestone Architecture

### P1.1 - Cargo / `RUSTC_WRAPPER` Interception

Cargo supports an environment variable `RUSTC_WRAPPER` pointing to an executable. When set, Cargo invokes the wrapper instead of invoking `rustc` directly, passing the path to the real `rustc` binary as the first argument followed by the standard compiler flags:

```text
cargo-instrument <path-to-rustc> [rustc-arguments...]
```

#### Key Implementation Components ([wrapper.rs](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/cargo-instrument/src/wrapper.rs)):
- **Dual-mode dispatcher ([main.rs](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/cargo-instrument/src/main.rs)):** Detects whether the process was invoked as `RUSTC_WRAPPER` (identifying `rustc` in argv[1] or via environment) versus user CLI invocations (`cargo instrument analyze <file>` or `cargo instrument -- <cargo args...>`).
- **Compiler argument preservation:** All original arguments passed from Cargo are preserved without mutation and forwarded verbatim to the real `rustc`.
- **Exit status propagation:** The exact exit code of the real `rustc` process is returned to Cargo. Compiler errors and warnings bubble up transparently.
- **Process recursion guard & logging:** The wrapper sets `CARGO_INSTRUMENT_ACTIVE=1` before spawning direct compiler children. When active, subsequent nested calls within that process branch skip re-analysis. All debug logs are tagged with PID and crate name (`[cargo-instrument PID={pid} crate={crate_name}]`).
- **Target-dir isolation check (ADR-004 / H2):** When the wrapper is invoked directly (without the CLI setting `CARGO_INSTRUMENT_WRAPPER_MODE`), it inspects `--out-dir`. If `--out-dir` does not contain `instrumented`, it emits an explicit warning to `stderr` alerting the operator that build cache isolation is inactive.

---

### P1.2 - Compilation-Unit Classification & Source Resolution

Not all invocations received by `RUSTC_WRAPPER` represent application crates eligible for instrumentation. In a standard build, Cargo issues multiple queries, compiles procedural macros on the host, and builds build scripts (`build.rs`).

#### Classification Rules ([discovery.rs](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/cargo-instrument/src/discovery.rs)):
The argument parser categorizes each compiler invocation into a strongly-typed `CompilationUnit`:

| Compilation Unit | Classification Criteria | Tool Action |
|---|---|---|
| `RustCrate` | Standard crate compilation with `--crate-name`, edition, and `.rs` source | **Eligible for AST analysis** |
| `BuildScript` | `--crate-name` is `build_script_build` or `build_script_*`, or source file is `build.rs` | Pass through to `rustc` without analysis |
| `ProcMacro` | `--crate-type proc-macro` present in argv | Pass through to `rustc` without analysis |
| `CompilerQuery` | Flags such as `-vV`, `--version`, `--print=...`, or input `-` (stdin) | Pass through to `rustc` immediately |
| `PassThrough` | No `.rs` source file found and no recognized query flags | Pass through safely |

#### Flag & Path Parsing:
- Extracts `--crate-name`, `--crate-type`, `--edition`, `--target`, `--out-dir`, and `--test`.
- Supports both space-separated (`--flag value`) and equals-separated (`--flag=value`) syntax.
- Correctly handles whitespace, path delimiters, and escaped characters across Windows, Linux, and macOS.

---

### P1.3 - AST Parsing, Module Resolution & Byte-Span Analysis

For eligible `RustCrate` units, the tool reads the crate's root source file into an immutable UTF-8 buffer and parses it using `syn::parse_file`.

#### 1. Root-Aware Recursive Module Discovery (C1)
A crate's AST does not reside in a single file when submodules are declared out-of-line (`mod foo;`). Module resolution rules depend strictly on whether the file being analyzed is the **crate root**:
- **Root threading (`is_root: bool`):** The entry point `analyze_source_file` passes `is_root: true` for the initial compilation unit file (e.g. `src/main.rs`, `src/lib.rs`, `src/bin/tool.rs`, `tests/integ.rs`).
- **Sibling resolution for crate roots:** For any crate root, child modules resolve as siblings in `root_file.parent()`. This ensures non-standard crate roots like `src/bin/tool.rs` resolve `mod bar;` to `src/bin/bar.rs` (or `src/bin/bar/mod.rs`), rather than incorrectly nesting under `src/bin/tool/bar.rs`.
- **Nested module resolution:** For non-root files, if the file is named `mod.rs`, child modules resolve in `current_file.parent()`; otherwise they resolve in `current_file.parent().join(file_stem)`.
- **Attribute overrides:** Supports `#[path = "..."]` relative to the current file's parent.
- **Inline modules:** Recursively visits items declared within inline modules (`mod inline { ... }`), adjusting search paths for nested out-of-line declarations.
- **`#[cfg(...)]` handling:** In this milestone, all modules are followed unconditionally regardless of `#[cfg]` attributes to guarantee complete candidate discovery across platforms.

#### 2. Candidate Function Eligibility (§12.2 / §16.14)
The AST visitor ([ast.rs](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/cargo-instrument/src/ast.rs)) identifies three categories of function definitions:
1. **Free functions:** Functions defined at crate or module level (`FunctionKind::Free`).
2. **Inherent methods:** Methods defined in inherent impl blocks `impl Type { ... }` (`FunctionKind::InherentMethod`).
3. **Trait methods:** Methods defined in trait implementations `impl Trait for Type { ... }` (`FunctionKind::TraitMethod`).

#### 3. Strict Signature Exclusions
The following constructs are intentionally excluded per Phase 0 design rules:
- **`const fn`:** Cannot execute runtime side effects or telemetry span creation.
- **`extern "C"` / ABI functions:** FFI boundary functions cannot safely accept injected span wrappers.
- **Nested functions:** Functions defined inside function bodies are excluded per §12.2 / §16.14 to avoid double-wrapping.
- **Direct self-recursion (H3 / R7):** Functions that call themselves by name (unqualified `foo()`, `Self::foo()`, `crate::foo()`, `super::foo()`) are excluded to avoid infinite span generation. An intentional conservative false-positive on look-alike calls (`OtherType::helper()` inside `fn helper()`) is accepted and documented per R7.

#### 4. Sound R10 Idempotence Discrimination (C2)
To avoid double-instrumenting code that already emits OpenTelemetry spans:
- **Existing attributes:** Functions marked `#[instrument]` or `#[tracing::instrument]` are excluded.
- **`with_context` discrimination (Closure vs. Value):**
  - `anyhow::Context::with_context` takes a closure: `with_context<C, F>(self, f: F) where F: FnOnce() -> C`.
  - `opentelemetry::trace::FutureExt::with_context` takes a context value: `with_context(self, cx: Context)`.
  - If `call.args[0]` is a `syn::Expr::Closure`, the call is definitively `anyhow`/`eyre`, NOT OpenTelemetry. The enclosing function is preserved as an eligible candidate.
- **`.start()` corroboration:** Calls to `.start()` are only treated as OpenTelemetry span creation if the file imports `opentelemetry` / `Tracer`, or if the receiver variable is literally named `tracer`.
- **Exact symbol matching:** Replaced coarse `"tracer"` substring matching with explicit ABI symbols (`__otel_...`) and `opentelemetry` path calls.

#### 5. Unsafe Lint Policy Detection (R26)
Inner file-level attributes are inspected to distinguish `#![forbid(unsafe_code)]` from `#![deny(unsafe_code)]`:
- `UnsafePolicy::Forbidden`: Hard restriction; cannot be overridden by local `#[allow]`.
- `UnsafePolicy::Denied`: Can be overridden by scoped item-level `#[allow(unsafe_code)]`.
- `UnsafePolicy::Allowed`: Default policy.

#### 6. Exact Byte-Span Calculation
Using `proc_macro2::Span::byte_range()`, the tool records exact UTF-8 byte ranges `start..end` in the original file buffer for:
- `byte_range`: The entire function definition item.
- `body_byte_range`: The function body block `{ ... }`.

These ranges are guaranteed to satisfy `&source_text[start..end]` slicing without offset drift or character boundary errors.

---

## 3. Adversarial Review Findings & Resolutions

Following initial implementation, an adversarial review identified 8 findings (2 Critical, 3 High, 3 Medium). All were resolved:

| ID | Finding | Root Cause | Resolution |
|---|---|---|---|
| **C1** | Single-file discovery gap | Only the root file was parsed; child modules were ignored. | Implemented root-aware recursive module discovery with `is_root: bool` threading, `#[path]` support, and inline module traversal. |
| **C2** | Over-aggressive R10 false positives | Functions calling `anyhow.with_context`, `timer.start()`, or `ray_tracer` were skipped. | Added closure-vs-value discriminator for `with_context`, file-scoped OTel import check for `.start()`, and dropped bare `"tracer"` substring. |
| **H1** | Build script recursion reality | `build.rs` processes run independently from Cargo, bypassing wrapper process env vars. | Acknowledged and documented Phase 2 package-graph metadata deferral; added PID/crate diagnostic tagging. |
| **H2** | Unisolated target dir bypass | Wrapper did not alert when invoked directly with unisolated `--out-dir`. | Added check in `run_wrapper` emitting an ADR-004 warning if `CARGO_INSTRUMENT_WRAPPER_MODE` is unset and `--out-dir` is not isolated. |
| **H3** | Missed qualified recursion | `Self::foo` or `crate::foo` were not detected as recursive. | Updated `is_directly_self_recursive` to inspect path's final segment. |
| **M1** | Synthetic discovery tests only | Classification was only tested with synthetic flags. | Added unit test with real captured `cargo build -v` rustc invocation argv. |
| **M2** | Concurrency race on wrapper tests | Mutating `CARGO_INSTRUMENT_ACTIVE` caused race conditions in parallel tests. | Tagged all `run_wrapper` tests with `#[serial]` and added RAII env cleanup guards. |
| **M3** | Unasserted integration output | Captured stderr was not checked for discovered candidate names. | Added stderr assertions for candidate names (`compute`, `async_task`, `Controller::handle`) and PID tags. |

---

## 4. Verified Reproduction Proofs

### Proof 1: C1 Multi-File Submodule Discovery
```text
# Command:
cargo-instrument analyze scratch/c1_fixture/src/main.rs

# Discovered Candidates:
crate: main
source: .../scratch/c1_fixture/src/main.rs
unsafe_policy: Allowed

candidates:
  - main: bytes 14..65 (scratch/c1_fixture/src/main.rs)
  - greet: bytes 0..61 (scratch/c1_fixture/src/helpers.rs)
```
*Before C1, `greet` from `src/helpers.rs` was omitted. Now, both root and submodule functions are discovered with exact byte ranges and file origins.*

### Proof 2: C2 `anyhow::Context::with_context` Sound Discrimination
```text
# Command:
cargo-instrument analyze scratch/c2_fixture.rs

# Discovered Candidates:
crate: c2_fixture
source: .../scratch/c2_fixture.rs
unsafe_policy: Allowed

candidates:
  - load_config: bytes 0..114 (scratch/c2_fixture.rs)
```
*Before C2, `load_config` was falsely excluded by coarse `.with_context` matching. With closure discrimination, it is correctly identified as an eligible candidate.*

---

## 5. Milestone P1.4 - Surgical Byte-Range Source Transformation

Milestone P1.4 takes the candidates produced by the frozen P1.3 AST analysis and performs deterministic surgical byte splicing directly on the **original immutable UTF-8 source buffer**.

### Key Architectural Invariants & Adversarial Review Resolutions

1. **Exact File Association & Scoped API (C1):**
   Removed heuristic basename-only candidate matching. `transform_source_file` enforces strict canonicalized/normalized path equivalence (`paths_are_identical`). Additionally, `transform_source_file_scoped` provides a direct API for callers to supply pre-scoped candidate lists, eliminating cross-file collisions between files sharing identical basenames (e.g., `src/handlers.rs` and `src/admin/handlers.rs`).
2. **Safe Candidate-Level Fail-Open (H2 / S11):**
   Per-candidate defects (out-of-bounds ranges, mid-codepoint UTF-8 boundaries, missing opening braces, or overlapping ranges) are recorded in `plan.skipped` as structured diagnostics (`SkippedCandidate` with `SkipReason`), allowing valid candidates in the same file to be safely transformed. True file-level failures (I/O failures or attempts to modify sources in-place) remain hard errors.
3. **Pluggable Emitter Seam (H3 / ADR-006):**
   Separated candidate validation, edit planning, and edit application from replacement generation by introducing the `Emitter` trait. P1.4 provides `SentinelEmitter`, and P1.5 can substitute native OpenTelemetry generation without modifying the byte-splicing engine.
4. **Line-Ending Preservation (M1):**
   The transformation engine inspects the source's dominant newline style (`\r\n` vs `\n`) via `detect_line_ending` and ensures all generated replacements match the file's convention bit-for-bit.
5. **Structurally Constrained Idempotence (M3):**
   `body_starts_with_anchor_sentinel` verifies that the opening `{` is immediately followed by the anchor comment and sentinel statement. Arbitrary string literals containing the anchor substring inside the body do not falsely suppress instrumentation.
6. **Live `RUSTC_WRAPPER` Pipeline Integration (H1):**
   Integrated surgical transformation directly into the compiler wrapper:
   ```text
   Cargo ──► RUSTC_WRAPPER ──► Source Discovery ──► P1.3 Candidate Analysis
                                                              │
   real rustc ◄── Mirrored / Instrumented Tree ◄── P1.4 Source Transformation
   ```
   When candidates are identified, the crate source tree is mirrored into an isolated directory under `--out-dir` (`target/instrumented/.../instrumented_sources/<crate_name>/`), transformed in-place within the mirror, and the mirrored root is forwarded to `rustc`. Original sources remain 100% bit-for-bit untouched, preserving Cargo fingerprint invariants.
7. **Immutable Original-Buffer Indexing (ADR-002):**
   Every `ByteEdit` offset refers strictly to byte offsets in the original UTF-8 source buffer. The transformer constructs a new output buffer in a single pass without mutating the original input buffer.
8. **In-Place Modification Prohibition (S1/S2):**
   `transform_source_file` rejects `input_path == output_path` (including canonicalized path equivalence) with `TransformError::InPlaceModificationDisallowed`.
9. **Minimal Sentinel Representation:**
   The injected sentinel proves insertion mechanics and compilation across all function signatures (sync, async, generic, inherent, trait, diverging `!`, unsafe fn, empty body):
   ```rust
   /* __cargo_instrument_anchor: "{function_name}" */
   let _cargo_instrument_sentinel = ();
   ```

---

---

## 6. Milestone P1.5 - Native OpenTelemetry Synchronous Code Generation

Milestone P1.5 delivers native OpenTelemetry synchronous instrumentation code generation, substituting the P1.4 `SentinelEmitter` via the pluggable `Emitter` seam without modifying the core byte-splicing engine.

### Key Architectural Invariants & Implementation Details

1. **Pluggable Emitter Seam & Async Capability (`handles_async`)**:
   In accordance with ADR-006, the `Emitter` trait was extended with `emit_body_suffix` and `handles_async(&self) -> bool`:
   - `SentinelEmitter` inherits `handles_async() -> true` (preserving 100% of P1.4 behavior, including `test_async_function`).
   - `NativeOtelEmitter` overrides `handles_async() -> false`.
   - `TransformationPlan::build_with_emitter` filters candidates where `c.is_async && !emitter.handles_async()`, recording `SkipReason::AsyncDeferred` in `plan.skipped` and emitting a diagnostic note in the wrapper.
   - P1.6 async instrumentation will simply override `handles_async() -> true` with zero modifications to the splicing engine.

2. **Verified OpenTelemetry 0.32.0 Trace Lifecycle API Surface**:
   Generated code invokes the 9 verified OpenTelemetry 0.32.0 trace lifecycle APIs using fully-qualified trait paths to eliminate unused trait import warnings under `#![deny(warnings)]`:
   - Tracer acquisition: `opentelemetry::global::tracer("{crate_name}")`
   - Span construction: `opentelemetry::trace::Tracer::span_builder(&__otel_tracer, "{name}")`
   - Span kind: `.with_kind(opentelemetry::trace::SpanKind::Internal)`
   - Span start: `.start(&__otel_tracer)`
   - Context creation: `<opentelemetry::Context as opentelemetry::trace::TraceContextExt>::current_with_span(__otel_span)`
   - Context attachment: `__otel_cx.attach()` (non-Result) / `__otel_cx.clone().attach()` (Result)
   - Span status extraction: `opentelemetry::trace::TraceContextExt::span(&__otel_cx)`
   - Error status setting: `.set_status(opentelemetry::trace::Status::error(""))`

3. **Non-Result Synchronous Functions (Pure RAII Scope Cleanup)**:
   For functions returning `()`, arbitrary types `T`, generic functions, unsafe functions, and diverging functions (`-> !`):
   ```rust
   /* __cargo_instrument_anchor: "{name}" */
   let __otel_tracer = opentelemetry::global::tracer("{crate_name}");
   let __otel_span = opentelemetry::trace::Tracer::span_builder(&__otel_tracer, "{name}")
       .with_kind(opentelemetry::trace::SpanKind::Internal)
       .start(&__otel_tracer);
   let __otel_cx = <opentelemetry::Context as opentelemetry::trace::TraceContextExt>::current_with_span(__otel_span);
   let __otel_guard = __otel_cx.attach();
   // original body (suffix is empty)
   ```
   On normal return, early `return val;`, `?` operator propagation, or unwinding panics, `__otel_guard` drops in LIFO order, detaching the context and ending the span automatically.

4. **Result-Returning Functions (Error Status Interception & Clippy Hygiene)**:
   For functions returning `Result<T, E>`:
   ```rust
   /* __cargo_instrument_anchor: "{name}" */
   let __otel_tracer = opentelemetry::global::tracer("{crate_name}");
   let __otel_span = opentelemetry::trace::Tracer::span_builder(&__otel_tracer, "{name}")
       .with_kind(opentelemetry::trace::SpanKind::Internal)
       .start(&__otel_tracer);
   let __otel_cx = <opentelemetry::Context as opentelemetry::trace::TraceContextExt>::current_with_span(__otel_span);
   let __otel_guard = __otel_cx.clone().attach();
   #[allow(clippy::redundant_closure_call)]
   let __otel_res: Result<_, _> = (|| {
       // original body
   })();
   if __otel_res.is_err() {
       opentelemetry::trace::TraceContextExt::span(&__otel_cx)
           .set_status(opentelemetry::trace::Status::error(""));
   }
   __otel_res
   ```
   - `#[allow(clippy::redundant_closure_call)]` eliminates clippy linting failures on immediately-invoked closures.
   - `let __otel_res: Result<_, _>` pins the type for `.is_err()` on diverging bodies (`always_panics` / `panic!()`) without breaking `impl Trait` return types (avoiding E0562).
   - `.is_err()` eliminates `redundant_pattern_matching` warnings.
   - Symmetrical scope-end drop: eliminating explicit `drop(__otel_guard)` eliminates `drop_non_drop` warnings while LIFO scope drop cleans up `__otel_guard` automatically.
   - Leading underscore on `__otel_guard` suppresses unused variable warnings under `#![deny(warnings)]`.

5. **`opentelemetry` Dependency Gate (S11 Fail-Open)**:
   `discovery.rs` parses `--extern` flags (both `--extern name` and `--extern name=path`). In `wrapper.rs`, if native OpenTelemetry mode is active and `has_opentelemetry` is false, the wrapper logs a diagnostic warning and compiles the original crate source unmodified, preserving S11 fail-open behavior and preventing E0433 errors.

6. **InstrumentationScope Per-Crate Attribution**:
   Tracer acquisition uses the instrumented crate's canonical rustc name (`opentelemetry::global::tracer("{crate_name}")`), preserving OpenTelemetry `InstrumentationScope` attribution across multi-crate builds.

7. **§16.14 Attribute Boundary & Rationale**:
   `code.function.name`, `code.file.path`, and `code.line.number` attributes require `opentelemetry::KeyValue` and are deferred to P1.8 (End-to-End Validation / Telemetry Export) where in-memory/OTLP span exporter and collector harnesses provide verification visibility.

8. **Span Name Normalization & Parameter Consistency**:
   - `ast.rs` normalizes token-stream formatting in `Candidate.function_name` and `Candidate.kind` via `normalize_type_str`, collapsing stray spaces around `::`, `<`, `>`, and `>>` while preserving `" as "` (e.g. producing `<MyErr as From<std::num::ParseIntError>>::from` conforming strictly to §16.14).
   - Standardized `transform_source_str_with_native_otel(source, crate_name, candidates)` and `TransformationPlan::build_with_native_otel(source, crate_name, candidates)` with leading `source: &str`, matching all sibling transformation helpers.

9. **Reference and Explicit Lifetime Fallback (Adversarial C1 & Type-Aliased &mut)**:
   When a function returns a mutable reference (e.g. `Result<&mut T, E>` or `&mut T`), wrapping the original body in a closure causes rustc to infer an `FnMut` closure where captured references cannot escape (`error: captured variable cannot escape FnMut closure body`). Furthermore, mutable references hidden behind type aliases (e.g. `pub type MutName<'a> = &'a mut String; Result<MutName<'_>, E>`) are invisible to a naive `&mut` syntactic scan.
   To eliminate this entire build-break class, `ast.rs` inspects the return type via `returns_reference_or_lifetime(output)` and flags `Candidate.returns_reference_or_lifetime`. Any non-static reference (`&`) or explicit non-static lifetime argument (`'_`, `'a`, etc.) triggers fallback to prefix-only instrumentation (pure RAII scope cleanup via LIFO guard drop) without closure wrapping. §16.10 error-status recording is a SHOULD, so falling back to prefix-only strictly conforms to normative specifications.
   *Safety & Future Tuning (Phase 2)*: Purely syntactic AST analysis cannot resolve type aliases with zero lifetime parameters (e.g. `type StaticMut = &'static mut String;`), though moving such a value out leaves the closure `FnOnce` and compiles cleanly; reborrowing behind a lifetime-less alias is close to unreachable in practice. Conversely, bare shared references (`Result<&str, E>`) currently degrade to prefix-only; in Phase 2, coverage can be recovered by refining the fallback to `&mut` anywhere or path types carrying lifetime arguments, preserving full error status on `Result<&str, E>` while continuing to safely protect `MutName<'_>`.

10. **Runtime Span Execution Proofs (Adversarial H1)**:
    Added `opentelemetry_sdk = { version = "0.32.0", features = ["testing"] }` to `[dev-dependencies]` and introduced live execution proofs using `InMemorySpanExporter`. These tests execute real instrumented functions and verify:
    - Exactly 1 span created per call.
    - Span kind is `SpanKind::Internal`.
    - Instrumentation scope matches the crate name (`proof_crate`).
    - Fallible calls returning `Err` record `Status::error("")`.
    - Successful calls returning `Ok` and non-Result calls leave `Status::Unset`.

11. **Direct Architecture Data Flow (Adversarial M1)**:
    Removed vestigial `InstrumentationIntent`. The operational compilation pipeline is directly:
    ```text
    Candidate ──► Emitter ──► ByteEdit
    ```

12. **Per-Invocation Tracer Acquisition Cost (Adversarial M2)**:
    `NativeOtelEmitter` generates `let __otel_tracer = opentelemetry::global::tracer("{crate_name}")` inside each instrumented function body. On each call, this performs a global `RwLock` read, an `Arc` clone, and a `Box` allocation (`BoxedTracer`). In accordance with §16.3 ("Not Phase 1 — measure first"), caching the tracer via `static` or `OnceLock` is deferred to Phase 2 performance profiling.

13. **Fail-Open Gating Scope (Adversarial M3)**:
    Fail-open to uninstrumented source occurs when native OpenTelemetry enforcement is requested (`CARGO_INSTRUMENT_NATIVE_OTEL=1`) and `opentelemetry` is missing from `--extern`. When not enforced and `opentelemetry` is absent, the wrapper safely defaults to `SentinelEmitter`, ensuring backward compatibility for legacy non-OTel compilation units.

14. **CRLF Line-Ending Preservation (Adversarial L1)**:
    Added `test_native_otel_crlf_preservation` verifying that `NativeOtelEmitter` preserves `\r\n` line endings bit-for-bit without introducing orphan `\n` characters.

---

## 7. Milestone P1.6 - Native OpenTelemetry Asynchronous Code Generation

Milestone P1.6 delivers native OpenTelemetry asynchronous code generation for `async fn` items, wrapping future execution with `opentelemetry::trace::FutureExt::with_context` without requiring any AST byte-splicer changes.

### Key Architectural Invariants & Lifecycle Semantics

1. **The Core Async Challenge: The `!Send` Context Trap**:
   In Rust, an `async fn` body compiles into an anonymous generator state machine. A naive RAII span guard (`__otel_guard`) created at the top of the function lives across `.await` suspension points. Because `opentelemetry::ContextGuard` contains `PhantomData<*const ()>`, it is **`!Send`**, causing executor task spawning (e.g. `tokio::spawn`) to fail compilation (`E0277`). Furthermore, holding an attach guard across suspension leaks trace context onto the executor worker thread (**S6 violation**), falsely adopting unrelated concurrent tasks.

2. **Resolution via `opentelemetry::trace::FutureExt::with_context`**:
   `FutureExt::with_context(fut, cx)` moves context attachment inside `WithContext::poll`:
   - An `Arc<Context>` clone is attached at the start of each `poll()`.
   - The guard is dropped at the end of `poll()` when the future yields (`Pending`).
   - Because the guard is strictly poll-local and never held across suspension, `WithContext<F>: Send` whenever `F: Send`.
   - Trace context is current exclusively while the task executes on a worker thread and absent while suspended.

3. **Zero Splicer Changes**:
   The two-point byte splicer from P1.4 directly produces this wrapped future architecture:
   - `emit_body_prefix` (inserted after `{`): Starts the span, creates `__otel_cx`, and opens `opentelemetry::trace::FutureExt::with_context(async move {`.
   - `emit_body_suffix` (inserted before `}`): Closes `}, __otel_cx).await` (with post-await status recording for `Result`).

4. **Normative Async Lifecycle**:
   | Lifecycle Stage | Runtime Behavior | Invariant Enforced |
   |---|---|---|
   | **Future construction** | Body does not run; zero tracer calls, zero spans created | §16.7 / SQ1 / FE-4 |
   | **First `poll()`** | Prefix executes on worker thread: tracer acquired, span started, `WithContext` constructed and polled | S1 (single span start) |
   | **Yield (`Poll::Pending`)** | `_guard` drops at end of `poll()`; context detached from thread; span clock continues | S6 (no cross-task pollution) |
   | **Resume (`poll()` on any worker)** | Context re-attached from stored `Context` on current worker thread | §16.5 (thread migration safety) |
   | **Ready (`Poll::Ready`)** | Guard drops; status recorded on Result path; `WithContext` drops → span ends | S1 (single span end) |
   | **Drop / Cancellation** | `WithContext` drops mid-flight → `Context` drops → SDK `Span::drop` hook ends and exports | §16.12 (cancellation export) |

5. **Generated Code Shapes**:
   - **Async Non-Result Functions**:
     ```rust
     /* __cargo_instrument_anchor: "{name}" */
     let __otel_tracer = opentelemetry::global::tracer("{crate_name}");
     let __otel_span = opentelemetry::trace::Tracer::span_builder(&__otel_tracer, "{name}")
         .with_kind(opentelemetry::trace::SpanKind::Internal)
         .start(&__otel_tracer);
     let __otel_cx = <opentelemetry::Context as opentelemetry::trace::TraceContextExt>::current_with_span(__otel_span);
     opentelemetry::trace::FutureExt::with_context(async move {
         // original body
     }, __otel_cx).await
     ```
   - **Async Result-Returning Functions**:
     ```rust
     /* __cargo_instrument_anchor: "{name}" */
     let __otel_tracer = opentelemetry::global::tracer("{crate_name}");
     let __otel_span = opentelemetry::trace::Tracer::span_builder(&__otel_tracer, "{name}")
         .with_kind(opentelemetry::trace::SpanKind::Internal)
         .start(&__otel_tracer);
     let __otel_cx = <opentelemetry::Context as opentelemetry::trace::TraceContextExt>::current_with_span(__otel_span);
     let __otel_res: Result<_, _> = opentelemetry::trace::FutureExt::with_context(async move {
         // original body
     }, __otel_cx.clone()).await;
     if __otel_res.is_err() {
         opentelemetry::trace::TraceContextExt::span(&__otel_cx)
             .set_status(opentelemetry::trace::Status::error(""));
     }
     __otel_res
     ```

6. **Unified Error and Return Handling**:
   In Rust, both early `return` and the `?` operator inside an `async move` block evaluate the inner block to `Result<T, E>` and complete the block's future. Awaiting the block yields `__otel_res: Result<_, _>`, which is inspected for `.is_err()` and recorded via `set_status(Status::error(""))`. `__otel_cx.clone()` (an Arc clone) preserves context access for the post-await status update.

7. **Exclusion of Async from Reference-Return Fallback (F3)**:
   In synchronous P1.5, `(|| { ... })()` triggered closure escape errors when returning `&mut` references, requiring fallback to prefix-only instrumentation (`returns_reference_or_lifetime`). In async functions, `async move { ... }` produces a generator that shares lifetime bounds with the outer future. Functions returning `Result<&mut T, E>` compile cleanly under `rustc -D warnings`. Therefore, `returns_reference_or_lifetime` is strictly exclusive to the synchronous path; async branches test `returns_result` only.

8. **Clippy & Compiler Hygiene (F1)**:
   The async block is an argument to `with_context(...)`, not a directly-awaited future, so `clippy::redundant_async_block` does not fire. No superfluous `#[allow]` attributes are injected into generated code.

9. **Empirical Verification Proofs**:
   - **Span duration (Matrix item 11 / §16.7)**: A 100 ms sleep inside an instrumented future produces span duration >= 90 ms (empirically measured ~109 ms), proving span duration measures wall-clock time across suspension points rather than ~0 ms CPU-busy time.
   - **`#[async_trait]` compatibility (R1)**: Verified that source spliced with `with_context(async move { ... })` compiles cleanly under `#[async_trait]` macro expansion and emits normalized `<Type as Trait>::method` spans.

10. **Deferred Scope**:
    - **Tier-2 async dependency instrumentation**: Scoped to Phase 2 (§12.3 / §16.3 / FE-13).
    - **Stream / Sink item instrumentation**: Deferred to Phase 2.
    - **Cross-task `tokio::spawn` propagation (§16.8)**: Deferred to Phase 2.
    - **Cancelled-vs-completed span status distinction (§16.12)**: Deferred to Phase 2.

---

### P1.7 - Dependency Instrumentation & `extern "C"` Trampolines

Milestone P1.7 extends instrumentation across third-party Cargo crate boundaries without manifest mutation or Cargo dependency injection using `extern "C"` ABI trampolines ([ADR-002](../research/17-decision-records.md), [ADR-003](../research/17-decision-records.md), [§12.1a](../research/12-mvp-definition.md), [§16.3](../research/16-instrumentation-semantics.md)).

#### Key Architectural Components:

1. **Standalone Runtime Shim Crate (`otel-shim`)**:
   - Resides as an independent workspace crate exporting a standardized C ABI on top of the native OpenTelemetry SDK (`opentelemetry` 0.32).
   - Exported symbols:
     - `__otel_span_enter`: Starts an internal span and attaches its context, returning an opaque `u64` handle.
     - `__otel_span_exit`: Pops context guard matching handle from thread-local stack and ends the span.
     - `__otel_span_set_error`: Handle-accurate error status recording (`Status::error("")`) per §16.10.
     - Async stubs (`__otel_span_start`, `__otel_span_end`, `__otel_ctx_attach`, `__otel_ctx_detach`): Accept handle 0 and return 0 per S9.
   - **Thread-Local LIFO Context Stack (C1 / F2)**:
     - Uses `RefCell<Vec<(u64, Context, ContextGuard)>>` in thread-local storage, enforcing S5 LIFO discipline and matching exact handles for error attribution without global lock contention or `!Send` context guard issues.
   - **Clippy & Safety Doc Hygiene (F1)**:
     - Every exported `unsafe extern "C"` function carries an explicit `/// # Safety` section documenting caller obligations (valid handle from prior enter or 0, valid UTF-8 pointer/len), compiling cleanly under `cargo clippy --workspace --all-targets -- -D warnings`.
   - **Extern Crate Pruning Prevention (ADR-003 / E-10)**:
     - Exports safe `otel_shim::init()`. Calling this in application code establishes a genuine Rust item-path reference, preventing `rustc` from dead-stripping `libotel_shim.rlib` at link time.

2. **Splicing Trampoline Emitter (`TrampolineEmitter`)**:
   - Implements `Emitter` with `handles_async() -> false`, gracefully deferring async candidates in dependencies per §12.3 / §16.3 / FE-13.
   - **Minimal Block-Scoped Declarations (M1)**:
     - Emits only the exact symbols needed per site (2 symbols for non-Result: enter + exit; 3 symbols for Result: enter + exit + set_error).
   - **Edition-Aware Syntax**:
     - Emits `extern "C"` for Edition 2015–2021 and `unsafe extern "C"` for Edition 2024.
   - **Unsafe Policy Handling (G3 / S11)**:
     - For `UnsafePolicy::Denied`, injects scoped `#[allow(unsafe_code)]` at declarations, drop guard, and set_error sites to recover coverage.
     - For `UnsafePolicy::Forbidden`, skips instrumentation per S11 fail-open ([§16.3](../research/16-instrumentation-semantics.md)).
   - **Type Collision Immunity**:
     - Uses fully-qualified `core::result::Result<_, _>` to avoid collisions with crate-local Result type aliases.

3. **Compilation-Unit Role Classification & Application Preflight**:
   - Strongly classifies units into `Application`, `WorkspaceMemberDependency`, `LocalPathDependency`, and `RegistryDependency`.
   - Evaluates `.cargo/registry`, `.cargo/git`, `opentelemetry*`, `cargo_instrument`, and `otel_shim` names prior to dependency flag inspection, ensuring telemetry runtime and registry dependencies are never classified as applications (`role()` explicitly excludes `otel_shim` so the runtime never instruments itself).
   - **Staging Decision on Registry Dependencies (§12.1, §12.3)**:
     - In Phase 1, the C-ABI trampoline mechanism is proven on out-of-workspace dependencies (`LocalPathDependency` and `WorkspaceMemberDependency`). Arbitrary third-party registry crates (`.cargo/registry`) are skipped as an explicit Phase 1 staging boundary to avoid unlinked host build-script tools and macro-heavy graphs, which are scheduled for Phase 2. Note: §12.1a and ADR-002/003 define the C-ABI mechanism and wrapper architecture; skipping registry crates is purely a Phase 1 staging boundary per §12.1 and §12.3.
   - Application crates declaring `otel-shim` undergo recursive module preflight (`check_application_preflight`) verifying an item path to `otel_shim::init()` before compiling dependencies.

4. **Source Byte Immutability (S1 / S2)**:
   - Dependency sources in out-of-tree and in-tree paths remain 100% bit-for-bit identical before and after instrumentation. Out-of-tree sources are mirrored relative to `scan_dir` while in-tree sources preserve H1 relativization guarantees.

---

## 8. Verification Matrix

The Phase 1 implementation is verified by **124 automated tests** across 9 test suites:

| Test Suite | Tests | Scope |
|---|---|---|
| [`ast_tests.rs`](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/cargo-instrument/tests/ast_tests.rs) | 24 | Free/inherent/trait functions, async, generics, exclusions, idempotence, unsafe policies, module resolution (root, non-main, nested, path attr), Result return detection, `&mut` detection (C1), type-aliased lifetime detection, span name normalization, error handling |
| [`discovery_tests.rs`](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/cargo-instrument/tests/discovery_tests.rs) | 11 | Classification (ordinary crate, proc macro, build script, queries), real captured cargo argv, paths with spaces, `--extern opentelemetry` detection (separated, equals, noprelude), `--extern otel_shim` detection, compilation unit crate role classification (`Application`, `WorkspaceMemberDependency`, `LocalPathDependency`, `RegistryDependency`), error handling |
| [`wrapper_tests.rs`](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/cargo-instrument/tests/wrapper_tests.rs) | 5 | Config parsing, argument forwarding, exit code propagation, recursion guard, serial execution |
| [`byte_span_tests.rs`](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/cargo-instrument/tests/byte_span_tests.rs) | 4 | Exact UTF-8 buffer slicing, emoji/multibyte offsets, multiline formatting, comment preservation |
| [`transform_tests.rs`](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/cargo-instrument/tests/transform_tests.rs) | 35 | Surgical byte splicing, comments/formatting preservation, unicode offsets, exclusions, idempotence, overlap rejection, permutation invariance, rustc & Cargo compilation proofs, CLI transform, C1 cross-file basename collisions, H1 live wrapper pipeline and absolute source path handling, H2 fail-open skips, H3 emitter substitution, M1 CRLF preservation, M3 string literal idempotence, diverging `!`, unsafe fn, empty bodies |
| [`cargo_integration_tests.rs`](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/cargo-instrument/tests/cargo_integration_tests.rs) | 5 | Real wrapped Cargo subprocesses, multi-file discovery on disk, SHA-256 byte-for-byte source preservation, isolated target dir wiring, CLI analyze subcommand |
| [`native_otel_tests.rs`](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/cargo-instrument/tests/native_otel_tests.rs) | 24 | Native synchronous and asynchronous OpenTelemetry 0.32.0 generation: ordinary sync functions, inherent/trait methods, generics, unsafe fn, Result return with `clone().attach()`, `?` operator propagation, explicit early return, diverging bodies, `&mut` return prefix-only fallback (C1), type-aliased `&mut` fallback, CRLF preservation (L1), tracer acquisition scope, idempotence (splicer & AST), marker false-positive protection, async capability gating (`handles_async`), dependency gate fail-open, comments preservation, `InMemorySpanExporter` direct SDK proof (H1), live sync compilation under `rustc -D warnings` and `clippy -- -D warnings` with runtime span assertions, async non-Result shape, async Result shape, async `&mut` reference error status retention (F3), async inherent/trait/generic methods, async idempotence, async CRLF/Unicode preservation, and live multi-threaded Tokio runtime proof with `InMemorySpanExporter` verifying the complete 16-point async test matrix under `cargo clippy -- -D warnings` and `cargo test`. |
| [`trampoline_tests.rs`](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/cargo-instrument/tests/trampoline_tests.rs) | 10 | Tier 2 `extern "C"` trampoline emission and runtime execution: synchronous ordinary function trampoline shape, Result-returning function trampoline shape with status Error and empty description per §16.10, edition 2021 `extern "C"` vs edition 2024 `unsafe extern "C"`, `UnsafePolicy::Denied` scoped `#[allow(unsafe_code)]` injection (G3), `UnsafePolicy::Forbidden` skip per S11, async deferral per §12.3 / §16.3, reference return prefix fallback, application preflight check in root and recursive submodules, preflight failure diagnostic on missing `otel_shim::init()`, live end-to-end multi-threaded Tokio runtime proof with `InMemorySpanExporter` verifying dependency spans, parent hierarchy, active-after-completion cleanup S5, and 100% bit-for-bit source byte immutability. |
| [`otel-shim/src/lib.rs`](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/otel-shim/src/lib.rs) | 6 | Standalone runtime shim C-ABI invariants: S9 null handle 0 no-op / no state change, S5 out-of-order LIFO popping protection (pop-only-if-top), 3-level deep nesting cleanly emptied, unknown handle 9999 reverse lookup leaving top span status Unset in InMemorySpanExporter, pointer edge cases (null, zero-length, invalid UTF-8) returning 0 without UB or stack poisoning, and cross-thread handle isolation (thread A handle passed to thread B does not mutate thread B's stack). |

---

## 9. Status & Handoff to Milestone P1.8

### Milestone P1.7 Status: COMPLETE
Milestone P1.7 is implemented, verified, and passing all automated test suites with 0 compiler warnings, 0 clippy warnings, and 124/124 tests passing across the workspace (118 in `cargo-instrument`, 6 in `otel-shim`).

### Key Deliverables of P1.7:
1. **Standalone `otel-shim` Runtime Crate**:
   - Implements standardized C ABI (`__otel_span_enter`, `__otel_span_exit`, `__otel_span_set_error`, and async stubs) on OpenTelemetry SDK 0.32.
   - S9 null handle 0 checks on all exported entrypoints.
   - Thread-local 3-tuple `RefCell<Vec<(u64, Context, ContextGuard)>>` stack with LIFO matching for handle-accurate error attribution (C1 / F2).
   - Global `AtomicU64` handle counter ensuring cross-thread handle uniqueness and isolation.
   - Explicit `/// # Safety` doc comments on all exported symbols satisfying `clippy::missing_safety_doc` under `-D warnings` (F1).
   - Calling `otel_shim::init()` satisfies ADR-003 / E-10 to retain the runtime crate during `rustc` link-time pruning.
   - Excluded from instrumentation in `discovery.rs::role()` so the telemetry runtime never instruments itself.
2. **`TrampolineEmitter`**:
   - Minimal block-scoped `extern "C"` declarations per site (2 symbols for non-Result, 3 for Result) (M1).
   - Edition 2021 (`extern "C"`) vs edition 2024 (`unsafe extern "C"`).
   - `UnsafePolicy::Denied` scoped `#[allow(unsafe_code)]` injection (G3).
   - Fully qualified `core::result::Result<_, _>` to avoid collisions with crate-local Result type aliases.
   - `handles_async() -> false` deferral of async candidates to Phase 2 (§12.3 / §16.3 / FE-13).
3. **Application Preflight Verification**:
   - `check_application_preflight(crate_name, root_path)` traverses recursive module graph (`#[path]` aware) to verify an item path reference into `otel_shim` before compiling instrumented dependencies (ADR-003 / E-10 / G2 / C2).
4. **Source Byte Immutability**:
   - Dependency sources in out-of-tree and in-tree packages remain bit-for-bit unchanged before and after instrumentation.

### What Is Next: P1.8 - End-to-End Validation
Milestones P1.1–P1.7 have implemented the complete compile-time instrumentation pipeline for both application crates (Tier 1 native OTel) and out-of-workspace dependencies (Tier 2 C ABI trampolines).

Milestone P1.8 will perform end-to-end operational validation:
1. **Collector Export Validation**: Verify OTLP export from an instrumented application against a live OpenTelemetry Collector instance.
2. **Overhead & Performance Benchmarks**: Measure compile-time overhead and runtime span generation latency.
3. **Multi-Crate Application Validation**: Validate end-to-end builds across realistic multi-tier workspaces.
4. **Registry Crate Widening Decision & Validation**:
   > **Standing Note**: P1.7 proved the C-ABI trampoline mechanism, zero-code source mirroring, and link resolution on external dependencies (`LocalPathDependency` and `WorkspaceMemberDependency`). However, the headline differentiator (§9.6: instrumenting real crates.io dependencies like `hyper`/`sqlx`/`tonic` untouched) is not yet exercised on arbitrary `.cargo/registry` graphs due to the Phase 1 staging boundary (deferring unlinked host build-script tools and macro-heavy graphs per §12.1 and §12.3). In P1.8, we will decide whether targeted/selective widening past `.cargo/registry` belongs in P1.8's end-to-end validation suite or is scheduled for Phase 2's production dependency scheduler, ensuring clear ownership of proving the headline differentiator on real registry graphs.
