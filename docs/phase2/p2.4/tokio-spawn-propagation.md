# P2.4 — Tokio Spawn Context Propagation

**Status:** Complete. Red/green verified, comprehensive unit tests, and multi-crate integration proof passing.  
**Result:** Native OpenTelemetry compilation units automatically preserve OpenTelemetry context across `tokio::spawn` task-creation boundaries via call-site context capture without introducing synthetic task spans or modifying Tier-2 C-ABI units.

This document records the design, implementation, and empirical verification for [Issue #6](https://github.com/darkraider01/rust-compile-time-instrumentation-/issues/6).

---

## 1. Problem Statement & Motivation

In asynchronous Rust programs, tasks spawned via `tokio::spawn` are detached from the caller's call stack and scheduled onto a thread pool runtime:

```rust
// Caller thread (has active ambient OpenTelemetry span / Context::current())
tokio::spawn(async {
    // Worker thread (executes in a fresh worker TLS without ambient caller context)
    dependency::work().await;
});
```

Because OpenTelemetry context in Rust is carried in thread-local storage (`Context::current()`), scheduling a task onto Tokio's work-stealing queue causes the spawned future to be polled on worker threads where the caller's context is inactive. Without task-boundary propagation, any spans created inside the spawned future receive `parent_span_id: SpanId::INVALID` and a newly generated `TraceId`, fragmenting distributed traces.

### Correctness Composition

Issue #6 builds directly upon the deterministic proof established in [Issue #5](async-context-propagation.md):

```text
Issue #6:
Caller context synchronously captured at spawn call site before task submission
                               +
Issue #5:
FutureExt::with_context safely reattaches stored context on every poll,
including across arbitrary OS thread migration
                               =
Robust, continuous context propagation regardless of runtime task scheduling
```

---

## 2. Architecture & Design Principles

### 2.1 Conservative Syntactic Recognition (No False Positives)

Because `cargo-instrument` operates via `syn` AST analysis rather than full `rustc` name resolution, spawn recognition is strictly conservative:

* **Recognized Patterns:**
  * `tokio::spawn(arg)`
  * `tokio::task::spawn(arg)`
  * `::tokio::spawn(arg)`
  * `::tokio::task::spawn(arg)`
* **Deliberately Excluded Patterns:**
  * Bare `spawn(arg)` (even if `use tokio::spawn` is present in the file, to avoid collisions with local functions, shadowing, or other libraries)
  * `task::spawn(arg)`
  * `std::thread::spawn(...)`
  * Unrelated crate calls (e.g. `other::spawn(...)`)
  * `tokio::task::spawn_blocking(...)`
  * Calls with argument count $\ne 1$

### 2.2 Structural Idempotence Detection

A spawn site is considered already instrumented if and only if its **top-level** argument expression is an OpenTelemetry context wrapper:
* `FutureExt::with_context(fut, cx)`
* `fut.with_context(cx)`

Inner statements inside the spawned future (such as `anyhow::Context::with_context` or nested async calls) do not trigger false-positive skipping; the spawn boundary itself is still wrapped.

### 2.3 Pure Insertion Edit Model

The transformation treats the original source buffer as strictly immutable. Wrapping a spawn argument consists of two pure insertion edits:
* Prefix insertion at `arg.start`: `opentelemetry::trace::FutureExt::with_context(`
* Suffix insertion at `arg.end`: `, opentelemetry::Context::current())`

Because function call arguments are evaluated synchronously on the caller's thread *before* entering `tokio::spawn`, `opentelemetry::Context::current()` captures the ambient span context at the exact moment of task dispatch.

Candidate function edits and spawn edits are merged into a single edit stream, validated for UTF-8 character boundaries, globally sorted by source offset, and checked for non-overlapping monotonicity before atomic application.

### 2.4 Pipeline Threading & Spawn-Only Files

Every `SpawnSite` records its canonical `source_file` and argument byte range. In `wrapper.rs`:
* Compilation unit eligibility checks `!report.candidates.is_empty() || !report.spawn_sites.is_empty()`.
* Mirroring groups spawn sites by canonical file alongside function candidates.
* Files containing candidates, spawn sites, or both are transformed into the mirror directory.
* Submodules containing Tokio spawns but zero instrumented functions (e.g. inline-only or helper files) are correctly mirrored and rewritten.

### 2.5 Strict Scoping to Native OpenTelemetry

Issue #6 is explicitly scoped to native OpenTelemetry units:
* `NativeOtelEmitter::handles_tokio_spawn(&self) -> bool { true }`
* `TrampolineEmitter::handles_tokio_spawn(&self) -> bool { false }`
* `SentinelEmitter::handles_tokio_spawn(&self) -> bool { false }`

The Tier-2 C-ABI fallback (`TrampolineEmitter`) remains completely untouched and does not emit native OpenTelemetry types or create Cargo dependency requirements for fallback crates.

### 2.6 Zero Synthetic Task Spans

The wrapper does not inject synthetic intermediate spans (e.g. `tokio.spawn` or `task.run`). Spans created by downstream functions inside the spawned task attach directly to the caller's active span:

$$\text{app\_parent} \longrightarrow \text{dep\_r4::async\_work}$$

---

## 3. Verification & Evidence

### 3.1 Unit & Transformation Test Suite (`tokio_spawn_tests.rs`)

A dedicated integration test suite in `cargo-instrument/tests/tokio_spawn_tests.rs` covers all design criteria:

1. `test_uninstrumented_tokio_spawn_loses_parent_context`: Demonstrates that raw `tokio::spawn` loses context on multi-threaded runtimes, producing `SpanId::INVALID`.
2. `test_ast_conservative_tokio_spawn_recognition`: Proves that only unambiguous `tokio::spawn` / `tokio::task::spawn` paths match; bare `spawn`, `other::spawn`, and `std::thread::spawn` remain untouched.
3. `test_ast_structural_idempotence`: Proves top-level `with_context` wrappers are skipped, while inner `.with_context(...)` calls inside async blocks remain eligible.
4. `test_emitter_tokio_spawn_scoping`: Asserts `NativeOtelEmitter` handles spawns while `TrampolineEmitter` and `SentinelEmitter` reject spawn rewrites.
5. `test_transform_composition_with_candidate_and_spawns`: Verifies clean composition of function candidate prefix/suffix edits and spawn argument wrapping in the same file.
6. `test_transform_nested_tokio_spawns`: Verifies nested `tokio::spawn` calls are both wrapped without offset drift or overlapping edit collisions.
7. `test_transform_spawn_only_file_with_zero_candidates`: Verifies a file with zero eligible candidates (e.g. `#[inline]` functions) and an active spawn transforms cleanly.
8. `test_runtime_multi_thread_context_propagation`: Proves runtime multi-threaded propagation across nested spawns under an active parent span with zero active context leaks and exact trace ancestry.

### 3.2 End-to-End Multi-Crate Integration Proof (`r4_extern_injection_tests.rs`)

The full end-to-end integration proof (`r4_extern_injection_path_dependency_proof`) compiles real multi-crate workspaces with `cargo-instrument` as the compiler wrapper:

* Application crate `app` with `opentelemetry` dependency;
* Path dependency `dep_r4` with native injection;
* `tokio::spawn(async { dep_r4::async_work().await })` executed across a multi-thread Tokio runtime;
* Verified that `spawned_async.parent_span_id == parent.span_context.span_id()`;
* Verified that `spawned_async.span_context.trace_id() == parent.span_context.trace_id()`;
* Verified deterministic cross-thread migration for plain async and `Result` async dependency functions;
* Verified full suite execution in 53.72s.
