← [Project overview](../../README.md) | [Phase 0 Research](../research/README.md)

---

# Phase 1 — Compile-Time Instrumentation Tool (`cargo-instrument`)

**Milestones covered:** P1.1, P1.2, P1.3  
**Status:** P1.1–P1.3 Complete; P1.4 Next  
**Toolchain baseline:** Rust 1.97+ stable; Cargo 1.97+ stable; `syn` 2.0; `proc-macro2` 1.0; `quote` 1.0  
**Test suite status:** 43 automated tests passing across Linux, Windows, and macOS (0 failures, 0 clippy warnings)  

---

## 1. Executive Summary & Boundaries

Phase 1 translates the frozen architecture from [Phase 0](../research/README.md) into a working, stable-Rust compile-time instrumentation CLI and wrapper tool (`cargo-instrument`).

This document records the design decisions, implementation architecture, empirical findings, and verification proofs for the first three Phase 1 milestones:
- **P1.1 — Cargo / `RUSTC_WRAPPER` interception**
- **P1.2 — Source discovery & compilation-unit classification**
- **P1.3 — `syn` AST & exact byte-span analysis**

### Invariant Boundaries for P1.1–P1.3
Per Phase 0 normative specifications ([§16](../research/16-instrumentation-semantics.md)):
1. **Analysis-only boundary:** P1.1–P1.3 perform observation and candidate discovery only. Zero source code rewriting, zero code generation, and zero OTLP/runtime wiring are performed in these milestones.
2. **Byte-for-byte source preservation (S1/S2):** Every source file inspected remains bit-for-bit identical before and after tool execution (verified by SHA-256 pre/post snapshots).
3. **Fail-open resilience (S11):** Any failure in discovery, parsing, or analysis logs a warning/debug message and permits `rustc` to compile the original crate unhindered.
4. **Target directory cache isolation (ADR-004):** Instrumented builds target `target/instrumented` to prevent clobbering Cargo's uninstrumented build cache.

---

## 2. Milestone Architecture

### P1.1 — Cargo / `RUSTC_WRAPPER` Interception

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

### P1.2 — Compilation-Unit Classification & Source Resolution

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

### P1.3 — AST Parsing, Module Resolution & Byte-Span Analysis

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

## 5. Verification Matrix

The milestone implementation is verified by **43 automated tests** across 5 test suites:

| Test Suite | Tests | Scope |
|---|---|---|
| [`ast_tests.rs`](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/cargo-instrument/tests/ast_tests.rs) | 21 | Free/inherent/trait functions, async, generics, exclusions, idempotence, unsafe policies, module resolution (root, non-main, nested, path attr), error handling |
| [`discovery_tests.rs`](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/cargo-instrument/tests/discovery_tests.rs) | 8 | Classification (ordinary crate, proc macro, build script, queries), real captured cargo argv, paths with spaces, error handling |
| [`wrapper_tests.rs`](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/cargo-instrument/tests/wrapper_tests.rs) | 5 | Config parsing, argument forwarding, exit code propagation, recursion guard, serial execution |
| [`byte_span_tests.rs`](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/cargo-instrument/tests/byte_span_tests.rs) | 4 | Exact UTF-8 buffer slicing, emoji/multibyte offsets, multiline formatting, comment preservation |
| [`cargo_integration_tests.rs`](file:///c:/Users/branybuck/code/rust%20compile%20time%20instrumentation/cargo-instrument/tests/cargo_integration_tests.rs) | 5 | Real wrapped Cargo subprocesses, multi-file discovery on disk, SHA-256 byte-for-byte source preservation, isolated target dir wiring, CLI analyze subcommand |

---

## 6. Handoff to Milestone P1.4

### What Is Next: P1.4 — Surgical Source Transformation
Milestones P1.1–P1.3 establish that the tool can intercept compiler invocations, classify compilation units, discover all source files across a crate, and compute exact byte spans without modifying source code.

Milestone P1.4 introduces **byte-range source splicing**:
1. **Input:** Original immutable source buffer and candidate byte ranges (`Candidate::body_byte_range`).
2. **Mechanism:** Slice before body `{`, inject span entry/guard tokens, copy original body statements untouched, inject span exit tokens before closing `}`.
3. **Correctness invariant:** Transformed source must compile cleanly under `rustc`, while preserving original comments, docstrings, formatting, and line structures outside the spliced block.
4. **Idempotence:** Re-running transformation on previously transformed source must produce identical bytes.
