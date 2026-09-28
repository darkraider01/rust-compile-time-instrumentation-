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

### 2.1 Conservative Syntactic Recognition with Explicit Shadow Exclusions

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

Before accepting those syntactic forms, the analyzer excludes an entire source file if it finds a local binding named `tokio`, including modules, imports/aliases, extern-crate aliases, structs, enums, unions, traits, type aliases, and type parameters. This deliberately under-transforms independent scopes in such a file rather than risking a rewrite of a shadowed call. At wrapper time, Cargo's `--extern tokio` input is also required; without that authoritative dependency signal, discovered spawn sites are discarded.

### 2.2 Structural Idempotence Detection

A spawn site is considered already instrumented if and only if its **top-level** argument expression is the exact wrapper emitted by this project:
* `opentelemetry::trace::FutureExt::with_context(fut, cx)`

Neither a final path segment named `with_context` nor a method call named `.with_context(...)` is enough to suppress instrumentation: `custom_lib::with_context(...)` and `future.with_context(custom_state)` remain eligible. Inner statements inside the spawned future (such as `anyhow::Context::with_context` or nested async calls) also do not trigger skipping.

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

### 2.7 H3: Cargo-Authoritative Tokio Package Identity Verification

An active rustc `--extern tokio` binding name alone is **insufficient identity proof**. In Cargo, dependency renames allow an arbitrary foreign package to be bound to the crate name `tokio`:

```toml
[dependencies]
tokio = { package = "fake-runtime", path = "../fake-runtime" }
```

Under such a rename, the compiler receives `--extern tokio=...`, and the source may contain syntactic calls to `tokio::spawn(...)`. However, that API has no relationship to Tokio, and wrapping it with `opentelemetry::trace::FutureExt::with_context` would introduce broken dependencies or invalid types.

To resolve **H3**, `cargo-instrument` enforces an authoritative identity model distinguishing five distinct concepts:

| Concept | Definition | Example | Identity Role |
| :--- | :--- | :--- | :--- |
| **rustc extern binding name** | Flag passed to `rustc` (`--extern <name>=...`) | `tokio` | Necessary gate: compiler exposes binding. Insufficient alone. |
| **Cargo dependency binding name** | Alias declared in `Cargo.toml` / `resolve.nodes[].deps[].name` | `tokio` | Binds the current unit's local import space. |
| **Cargo package ID** | Globally unique package instance in resolve graph | `registry+...#tokio@1.43.0` | Disambiguates instance across graph. |
| **Cargo package name** | True package name in manifest `packages[].name` | `tokio` vs `fake-runtime` | Authoritative package identity proof. |
| **Source syntax** | Spelled path in source AST | `tokio::spawn(...)` | Syntactic candidate for call-site wrapping. |

#### Authoritative Invariant
Automatic Tokio spawn propagation runs **if and only if** all authoritative identity conditions are proven:
1. `rustc` actually supplies an extern binding named `tokio`.
2. The current wrapped unit resolves to an exact Cargo package ID via `package_manifest_dirs` longest-prefix match.
3. That Cargo package has a dependency edge in Cargo metadata `resolve.nodes[].deps[]` whose binding name is `tokio`.
4. That dependency edge resolves to an exact Cargo package ID.
5. That target package's true Cargo package name is `tokio`.
6. When authoritative Cargo `compiler-artifact` records are captured (during the H1 same-target pre-pass), an exact matching Tokio artifact exists:
   - Target and profile match the current compilation unit.
   - The compiled `.rlib` path matches rustc's `--extern tokio=<rlib>` via canonical path identity (`paths_refer_to_same_file`).
   - The active resolved features include the mandatory runtime feature (`"rt"`, either directly or transitively enabled by `"rt-multi-thread"`).
7. The spawn site passes conservative lexical, shadow, and structural idempotence checks.

If any link in this proof is missing, foreign, ambiguous, or lacks the required runtime features, `report.spawn_sites` is cleared and no rewriting occurs (conservative no-transformation).

#### Status & Resolution: H3 Complete

- **Status:** **COMPLETE**
- **Production pipelining follow-up (2026-09-28):** Cargo-reported `.rmeta` companions are validated alongside `.rlib` artifacts. Missing artifact records suppress wrapper rewrites even when package metadata proves a Tokio binding. The real public CLI regression in [closeout.md](closeout.md) proves spawn parenting through this path.
- **Cargo Package/Binding/Rename Identity:** The 6-point check establishes that the current unit's Cargo dependency edge named `tokio` maps to a genuine Cargo package whose package name is `tokio`. This decisively eliminates the dependency-rename vulnerability (`tokio = { package = "fake-runtime" }`), rejects packages that rename Tokio away, and protects units lacking a direct dependency on Tokio.
- **Authoritative Artifact & Feature Profile Identity:** During the same-target pre-pass, Cargo `compiler-artifact` JSON records are captured for `tokio` packages into `SessionPlan::tokio_artifacts`. In `SessionPlan::tokio_artifact_for`, the compiler wrapper validates:
  - Exact artifact match for target and profile;
  - Presence of the `.rlib` file on disk;
  - Satisfaction of required features (`"rt"`, including transitive satisfaction via `"rt-multi-thread"`);
  - Exact path equality / canonical path equivalence (`paths_refer_to_same_file`) against any active rustc `--extern tokio=<path>` binding.
  Any feature deficiency (e.g. Tokio compiled with only `sync` or `macros`), target mismatch, profile mismatch, non-existent artifact path, or ambiguity causes validation to fail safely, logging a clear diagnostic and suppressing spawn call-site rewrites.

---

## 3. Verification & Evidence

### 3.1 Unit & Transformation Test Suite (`tokio_spawn_tests.rs`)

A dedicated integration test suite in `cargo-instrument/tests/tokio_spawn_tests.rs` covers all 21 design criteria:

1. `test_uninstrumented_tokio_spawn_loses_parent_context`: Constructs and attaches a fixed, valid nonzero `SpanContext`, proves it is active before dispatch, then proves raw `tokio::spawn` sees `SpanId::INVALID` while the explicit wrapper preserves the exact fixed IDs. It has no global provider dependency.
2. `test_ast_conservative_tokio_spawn_recognition` and `test_ast_tokio_shadowing_excludes_syntactic_spawn_matches`: Prove only accepted paths match and that `mod tokio`, `use ... as tokio`, and `extern crate ... as tokio` suppress rewrites.
3. `test_ast_structural_idempotence`: Proves only the exact generated OpenTelemetry path is skipped; custom path and method calls named `with_context`, plus inner calls, remain eligible.
4. `test_emitter_tokio_spawn_scoping`: Asserts `NativeOtelEmitter` handles spawns while `TrampolineEmitter` and `SentinelEmitter` reject spawn rewrites.
5. `test_transform_composition_with_candidate_and_spawns`: Verifies clean composition of function candidate prefix/suffix edits and spawn argument wrapping in the same file.
6. `test_transform_nested_tokio_spawns`: Verifies nested `tokio::spawn` calls are both wrapped without offset drift or overlapping edit collisions.
7. `test_transform_spawn_only_file_with_zero_candidates`: Verifies a file with zero eligible candidates (e.g. `#[inline]` functions) and an active spawn transforms cleanly.
8. `test_runtime_multi_thread_context_propagation`: Proves runtime multi-threaded propagation across nested spawns under an active parent span with zero active context leaks and exact trace ancestry.
9. `test_h3_metadata_identity_proof_all_adversarial_cases`: Full multi-case adversarial unit suite verifying that real Tokio is proved, fake package renamed to `tokio` is rejected, Tokio renamed away to `my_tokio` is rejected, Tokio elsewhere in workspace is rejected, multiple Tokio versions resolve strictly per unit edge, and unknown units fail open safely.
10. `test_h3_adversarial_fake_runtime_renamed_to_tokio_live_cargo_build`: Live Cargo workspace integration proof where an application crate depends on `tokio = { package = "fake-runtime", path = "..." }` and calls `tokio::spawn`. Proves the wrapper detects the foreign package identity, logs suppression, leaves the spawn site unwrapped, transforms legitimate function candidates, and the binary compiles and runs cleanly.
11. `test_h3_valid_tokio_artifact_selected`: Proves that an exact matching Tokio artifact with `rt` feature, matching target and profile, and matching `--extern tokio` path passes validation.
12. `test_h3_adversarial_tokio_artifact_lacks_rt_feature`: Proves that a Tokio artifact compiled with only `["sync", "macros"]` (lacking `rt`) fails validation with `FeatureMismatch` and suppresses spawn wrapping.
13. `test_h3_tokio_artifact_rt_multi_thread_satisfies_rt_requirement`: Proves that Tokio compiled with `features = ["rt-multi-thread"]` (which resolves `rt` into the compiler-artifact features array) satisfies the `"rt"` requirement.
14. `test_h3_tokio_artifact_target_mismatch_fails`: Proves target mismatch between compilation unit and Tokio artifact causes validation failure (`TargetMismatch`).
15. `test_h3_tokio_artifact_profile_mismatch_fails`: Proves profile mismatch (e.g. debug vs release) causes validation failure (`ProfileMismatch`).
16. `test_h3_tokio_extern_artifact_path_identity_enforcement`: Proves exact path equality and canonical path equivalence are accepted, while different directories with matching filenames or non-existent/stale paths are rejected (`ExternPathMismatch`).
17. `test_h3_missing_tokio_artifact_features_fails`: Proves compiler-artifact messages lacking feature information fail validation (`MissingFeatures`).
18. `test_h3_multiple_ambiguous_tokio_artifacts_rejected`: Proves multiple conflicting Tokio artifacts for the same unit/profile fail safe (`Ambiguous`).
19. `test_h3_tokio_package_name_differs_from_bin_target`: Proves binary targets whose name differs from package name (e.g. package `my-service` with `[[bin]] name = "server"`) correctly validate active Tokio artifacts.
20. `test_h3_live_cargo_build_tokio_feature_safety`: End-to-end live Cargo build proof executing `cargo build` with `cargo-instrument` as `RUSTC_WRAPPER` against a real crate depending on real Tokio with only `features = ["sync", "macros"]` (lacking `rt`). Asserts suppression is logged, function candidates are transformed, the spawn site is left untouched, and the resulting binary runs and passes assertions.

### 3.2 End-to-End Multi-Crate Integration Proof (`native_artifact_injection_tests.rs`)

The full end-to-end integration proof (`r4_extern_injection_path_dependency_proof`) compiles real multi-crate workspaces with `cargo-instrument` as the compiler wrapper:

* Application crate `app` with `opentelemetry` dependency;
* Path dependency `dep_r4` with native injection;
* `tokio::spawn(async { dep_r4::async_work().await })` executed across a multi-thread Tokio runtime;
* Verified that `spawned_async.parent_span_id == parent.span_context.span_id()`;
* Verified that `spawned_async.span_context.trace_id() == parent.span_context.trace_id()`;
* Verified deterministic cross-thread migration for plain async and `Result` async dependency functions;
* Verified full suite execution in 53.72s.

### 3.3 Issue #6 Scope Checklist

The closed issue remains correctly closed. The following criteria are covered by the tests and the linked Issue #5 poll-time proof:

- [x] Multi-thread Tokio runtime.
- [x] Task migration between worker threads.
- [x] Nested spawns.
- [x] Spawned dependency work.
- [x] No thread-local identity assumptions.
- [x] No leaked contexts/spans.
