← [Project overview](../../README.md) | [Phase 0 Research](../research/README.md) | [Phase 1 Implementation](../phase1/README.md) | [Phase 2 ADRs (007–013)](decision-records.md)

---

# Phase 2 - Production Hardening

**Milestones:** P2.1, P2.2, P2.3, P2.4, P2.5
**Status:** In progress (P2.1 Steps 1–7 + Closeout D1–D5 complete; P2.2 coexistence complete; verified locally on Windows MSVC; CI matrix covers Ubuntu, Windows, macOS)
**Toolchain:** Stable Rust for the workspace (CI tracks latest `stable`; verified locally on 1.97.1). P2.3 generation alone uses the pinned toolchain in [`tools/p23-toolchain.txt`](../../tools/p23-toolchain.txt) plus `rustc-dev`, `rust-src`, and `llvm-tools-preview` (required for compiler-private linking on Windows); generated application source and the existing dependency wrapper pipeline remain stable-Rust consumers.
**P2.3 driver policy:** The current pin is `nightly-2026-09-09`. Repository-development invocation via `cargo instrument-rust --apply` is supported; installed/distributed driver discovery remains a deliberate follow-up.
**Baseline:** Phase 1 complete at [`409b774`](https://github.com/darkraider01/rust-compile-time-instrumentation/commit/409b774), 145 automated tests passing
**Phase 2 test suite status:** 11 graph-topology regression tests + 1 105-unit scale test + 5 hybrid coexistence tests added, all passing (176 tests total across the workspace)
**Architecture decisions:** [ADR-007 … ADR-013](decision-records.md), continuing the frozen Phase 0 numbering

---

## Phase Progression

```text
Phase 0 - Research & Architecture (Frozen)
          │
          ▼
Phase 1 - Compile-Time Instrumentation (Complete, P1.1–P1.8)
          │
          ▼
Phase 2 - Production Hardening (In Progress)
          │
          ├── P2.1 Unit Identity, Instrumentation Policy & Mirror Isolation  ✅ Complete
          │       ├── Step 1  Regression lock-down          ✅ Complete
          │       ├── Step 2  Unit identity (-C metadata)   ✅ Complete
          │       ├── Step 3  Mirror isolation & atomicity  ✅ Complete
          │       ├── Step 4  Instrumentation policy        ✅ Complete
          │       ├── Step 5  Dep-info scoping (G6)         ✅ Complete
          │       ├── Step 6  Preflight fail-open (G5)      ✅ Complete
          │       ├── Step 7  Scale & stress validation     ✅ Complete
          │       └── Closeout (D1–D5) Hardening            ✅ Complete
          ├── P2.2 Macro Expansion Resilience & Coexistence ✅ Complete
          │       ├── Attribute matcher widening (ADR-009)  ✅ Complete
          │       ├── Collision prevention (__otel_cx)      ✅ Complete
          │       ├── Hybrid parenting proof (ADR-010)      ✅ Complete
          │       └── Contain panic at C ABI boundary (R-3) ✅ Complete
          ├── P2.3 First-Party Lint-Apply (`cargo instrument-rust`) ◐ Bounded vertical slice complete
          │       ├── Spike: Body wrapping & `#[async_trait]` reachability (ADR-012) ✅ Complete
          │       ├── Step 1  `rustc_private` lint driver foundation
          │       ├── Step 2  HIR eligibility analysis & visit dedup
          │       ├── Step 3  `span_suggestion` transformation engine (`span_to_snippet`)
          │       ├── Step 4  CLI surface (`--show` preview, `--apply` clean-tree gate)
          │       └── Step 5  `cargo fix` round-trip & regression suite
          ├── P2.4 Opt-In Dependency Pipeline & Async Trampolines   ⬜ Planned
          │       ├── R-4 investigation: `--extern` injection vs C-ABI / R-1 / R-2
          │       ├── Async dependency trampolines & `tokio::spawn` context propagation
          │       ├── Stream / Sink instrumentation & span completion status
          │       └── Opt-in integration (`--with-dependencies` / env flag)
          └── P2.5 Large Graphs & Cross-Platform Validation         ⬜ Planned
                  ├── Multi-crate workspace scale & opt-in graph scale (≥100 units)
                  ├── Tracer caching (`OnceLock`)
                  └── Cross-platform verification: Windows MSVC (MAX_PATH), Linux ELF, macOS Mach-O
          │
          ▼
Phase 3 - Evaluation & Research (Planned)
```

---

## 1. Executive Summary

Phase 2 opens with a scope correction. The Phase 1 handoff named the first milestone a
**"Production Dependency Scheduler."** Investigation of the Phase 1 implementation
rejected that framing:

> **Cargo already is the scheduler.** It owns the dependency graph, topological
> ordering, unit deduplication, parallelism and freshness. `wrapper.rs` is - correctly -
> a stateless function of `(argv, filesystem, env)` invoked once per `rustc` call, and it
> has no ability to influence any of those decisions. Building a second scheduler would
> duplicate Cargo without authority over it.

What is actually missing is not scheduling but **identity and knowledge**:

1. the wrapper cannot tell two compilation units apart, because it keys everything on
   `--crate-name`, which Cargo reuses across package versions and across lib/test units; and
2. the wrapper cannot know whether the artifact that will eventually link a crate provides
   the `otel-shim` C-ABI symbols it splices into that crate.

P2.1 is therefore redefined as **Unit Identity, Instrumentation Policy & Mirror Isolation**.
It is a smaller milestone than the original framing and strictly higher value: four
build-breaking or silently-miscompiling defects follow directly from those two gaps, and
none of the later milestones can be validated while they stand.

A notable finding is that the fix for the identity half is close to free. Cargo already
passes each unit a globally unique key - `-C metadata=<hash>`, which encodes package,
version, source, features, profile and host/target kind - and
[`discovery.rs`](../../cargo-instrument/src/discovery.rs) currently parses and discards it
via the generic `-C` skip in `is_argument_consuming_flag`.

---

## 2. Reproduced Defect Register

Every defect below was reproduced against `409b774` with real `cargo`/`rustc`
subprocesses. Each has a corresponding regression test (§4).

### G1 - Mirror collision across package versions

`mirror_and_transform_crate_sources` derives the mirror path as
`<out-dir>/instrumented_sources/<crate-name>`. `--out-dir` is the single shared `deps`
directory for every unit in a build, and `--crate-name` is `foo` for both `foo v1` and
`foo v2`. Both units mirror into one directory.

Mirroring only adds and overwrites files - it never removes them - so the shared directory
accumulates the union of both versions' sources, with common files overwritten by whichever
`rustc` process ran last. One `foo` unit is then compiled from the *other* version's source.

**Observed on `409b774`,** with `foo v1::shared` adding 1 and `foo v2::shared` adding 2:

```text
expected: 2 3
actual:   3 3          # foo v1 was compiled from foo v2's mirrored source
exit code: 0           # no error, no warning
```

The same collision reproduced against real registry crates (`syn 1.0.109` + `syn 2.0.103`
in one graph) produced a 52-file mirror where the two versions have 46 and 49 files
respectively, mixing `await.rs`/`gen_helper.rs`/`reserved.rs` from v1 with
`classify.rs`/`fixup.rs`/`meta.rs` from v2, and failed the build outright:

```text
error: couldn't read …\instrumented_sources\syn\src\export.rs:
       The process cannot access the file because it is being used by another process. (os error 32)
```

**Severity: critical.** The silent-miscompilation outcome is worse than the crash, because
the instrumented binary behaves differently from the uninstrumented one with no diagnostic.
Note that this escapes S11 fail-open entirely: the wrapper *succeeded*; `rustc` failed after it.

### G2 - Host-side dependencies receive Tier-2 trampolines

`run_wrapper` selects `TrampolineEmitter` for any crate whose role is not `Application`.
That includes crates compiled for the **host** as dependencies of a procedural macro or
build script. Those units are linked into the proc-macro dynamic library, which does not
link `otel-shim`, so the spliced symbols have no provider.

**Observed on `409b774`** for `app -> pmmacro (proc-macro) -> pmdep`:

```text
libpmdep…rlib : error LNK2019: unresolved external symbol __otel_span_enter
                referenced in function _RNvCs160dLWov0T3_5pmdep5shout
pmmacro-….dll : fatal error LNK1120: 2 unresolved externals
```

This defect is **not solvable from argv**. In a non-cross build a host unit and a target
unit of the same package are argv-identical apart from `-C metadata`; there is no `--target`
flag to discriminate them. It requires build-graph knowledge, which is what makes it P2.1
scope rather than a local fix.

The defect is currently **masked by accident**: `syn 2.0.119`, `quote`, `proc-macro2`,
`thiserror` and `tracing` are all `#![no_std]` and are skipped by an unrelated filter
(`§12.3`). `syn 1.0.109` is not `no_std`, and it *is* instrumented.

### G3 - Mirror collision across units of one package

Cargo compiles `src/lib.rs` twice when tests are built - once as the library rlib, once as
a `--test` harness - passing the same `--crate-name` to both. Both mirror into one
directory, and each may observe the other's partially written files.

**Observed on `409b774`:** 2 units mirrored, 1 mirror directory created. This is the same
root cause as G1 but needs no multi-version or third-party graph at all, which makes it the
cheapest proof that `--crate-name` is not a compilation-unit identity.

### G4 - Trampolines emitted into a graph with no provider

Tier-1 native emission is gated on the crate actually depending on `opentelemetry` (S11
fail-open). Tier-2 has no equivalent gate: any non-application crate is spliced with
`extern "C"` trampoline declarations regardless of whether `otel-shim` appears anywhere in
the build graph.

**Observed on `409b774`** for `app (bin) -> dep_lib`, with `otel-shim` absent:

```text
liblibtest_fixture…rlib : error LNK2019: unresolved external symbol __otel_span_enter
fatal error LNK1120: 2 unresolved externals
```

Unlike G2 the failing link is an **executable**, which requires every symbol to resolve on
every supported platform, so this reproduces identically on MSVC, ELF and Mach-O.

### G5 - Application preflight hard-exits the build (observed, not yet pinned)

`run_wrapper` returns a failing exit code when a crate declaring `otel-shim` has no
syntactic `otel_shim` item path. This is the only place the tool deliberately breaks a
build, and it fires on a heuristic AST search.

It surfaced while building the G3 fixture: an integration-test target receives
`--extern otel_shim` like any other target in the package, so `tests/it.rs` was required to
carry its own `otel_shim::init()` reference or the build was failed outright. The G3 fixture
satisfies the check rather than working around it, so the test isolates the mirror collision.

**Not yet covered by a regression test.** P2.1 should downgrade this path to an S11 warning.

### G6 - `remap_dep_info_files` is O(N²) (scale, not correctness)

`remap_dep_info_files` reads **every** `.d` file in the shared `deps/` directory after
**every** successful instrumented compile. At 500 crates that is ~250,000 file reads, and it
writes into a directory that other `rustc` processes are concurrently populating. The
unit's own dep-info file name is derivable from `-C extra-filename`, which is already in argv.

### G7 - Mixed-provider workspace fails to link

**Reproduced 2026-09-10.** `has_otel_shim_provider` is a single boolean for the whole build graph -
`session.rs:512` is an `any()` over every target-reachable package. The property it needs to express
is per-package: *will every binary that links this rlib have the shim?*

Fixture: a workspace with `app_a` (depends on `otel-shim`), `app_b` (does not), and a shared
`common` that both link. `common` receives Tier-2 trampolines because the graph-wide flag is true,
and then `app_b` cannot resolve them:

```
error LNK2019: unresolved external symbol __otel_span_enter referenced in function common::shared_work
error LNK2019: unresolved external symbol __otel_span_exit referenced in ... __OtelGuard as Drop::drop
fatal error LNK1120: 2 unresolved externals
error: could not compile `app_b` (bin "app_b")
```

The same fixture builds clean without the wrapper. **The tool turns a working build into a failing
one**, which puts it in the same class as G4 and in direct conflict with S11 - every other failure
path warns and compiles unmodified, this one hard-fails, and no preflight warning fires first.

G4 does not catch this: G4 tests a graph with *no* provider anywhere, and passes. Here the graph
does contain a provider, just not on every path to `common`.

**Root cause is shared compilation, not detection.** `common` is compiled once and shared, so it
cannot be instrumented per-consumer. The gate is "instrument this package only if *all*
target roots reaching it provide the shim", which uses per-root reachability rather than the
former union set. The same shape recurs in
[ADR-011](decision-records.md#adr-011---the-tier-2-c-abi-is-provisional) for multi-version graphs:
one shared compilation, several consumers with incompatible requirements.

**Fixed in commit [`1bd0091`](https://github.com/darkraider01/rust-compile-time-instrumentation-/commit/1bd0091) (`fix(session): per-target-root otel-shim reachability for mixed-provider workspaces (G7)`).**
`SessionPlan` now runs a separate reachability BFS per target root (not one merged BFS across the whole graph), and any package reachable from a root that lacks `otel-shim` goes into a new `shim_unsafe_packages` set on `SessionPlan`, checked via `is_shim_unsafe()` in `wrapper.rs` alongside the existing `has_otel_shim_provider()` gate. If a package is marked unsafe, Tier-2 trampoline injection is skipped per S11 fail-open with a diagnostic warning, allowing unmodified compilation and clean linking.

A key implementation detail that mattered: `target_roots` itself had to be redefined too. A naive "run the existing BFS per member" would treat `common` as its own root and always mark it unsafe, breaking the working single-binary case. The actual fix defines a root as a workspace member producing a `bin`/`cdylib`, or one with no other workspace member depending on it (in-degree 0 in the workspace normal-dependency graph) — not just "every non-proc-macro member."

Covered by two regression tests in [`cargo-instrument/tests/graph_topology_tests.rs`](../../cargo-instrument/tests/graph_topology_tests.rs):
- `test_g7_mixed_provider_workspace_fails_open_for_common_dep` (the reproduction fixture — asserts the build succeeds, `common` receives no trampolines, and both binaries link and run);
- `test_g7_single_binary_with_shim_instruments_common_dep` (confirms the existing single-binary case doesn't regress — `common` receives Tier-2 trampolines, and `app_a` links and runs cleanly).

### P2.1 Closeout Defect Register (D1–D5)

Following initial P2.1 delivery, a closeout review surfaced five secondary defects in the caching, normalization, manifest discovery, and fail-open defaults of `SessionPlan`:

#### D1 - Session cache has no fingerprint, only a 1800s time window

`SessionPlan::load_or_create` originally checked only whether the session file was less than 1800 seconds old. If `Cargo.toml` was edited within this window:
- Removing `otel-shim`: The warm cache retained `has_otel_shim_provider: true`. The wrapper spliced trampolines into dependencies, causing `LNK2019: unresolved external symbol __otel_span_enter` (reinstating the G4 regression).
- Adding `otel-shim`: The warm cache retained `false`. Instrumentation was skipped silently with no warnings and no telemetry.

**Fix:** Replaced time window with a SHA-256 fingerprint over `Cargo.lock` and all workspace-member `Cargo.toml` manifests. Cache is rejected and rebuilt whenever manifests change. Covered by `test_d1_session_cache_fingerprint_invalidation_both_directions`.

#### D2 - Host-only package matching compares hyphenated names against underscored crate names

`host_only_packages` was populated with Cargo package names from metadata (`pm-dep`), but was queried using `--crate-name` from rustc argv, which always normalizes hyphens to underscores (`pm_dep`). The primary name match was dead for any package with a hyphen in its name.

**Fix:** Normalized both package names at metadata insertion and target crate queries to underscores (`replace('-', "_")`). Covered by `test_d2_hyphenated_proc_macro_host_dependency`.

#### D3 - `find_best_manifest_dir` is a heuristic that can pick the wrong manifest

In nested or multi-package workspaces, the directory walking heuristic in `find_best_manifest_dir` scored candidate `Cargo.toml` files by text keywords, potentially choosing a sibling or incorrect manifest when invoked via the CLI.

**Fix:** In `main.rs`, `execute_cargo_with_wrapper` now precomputes `SessionPlan::build_from_metadata` from the manifest directory specified via `--manifest-path` (or current directory if omitted), and passes it to worker processes via `CARGO_INSTRUMENT_SESSION`. `find_best_manifest_dir` remains strictly as a fallback for raw `RUSTC_WRAPPER` invocations. Covered by `test_d3_cli_session_plan_avoids_wrong_manifest_heuristic` and `test_d3_cli_session_plan_respects_manifest_path_flag`.

#### D4 - Parallel wrapper query storm on cold cache mitigated via CLI precomputation

Without CLI orchestration, the first wave of parallel `rustc` processes hitting a cold cache could concurrently shell out to `cargo metadata`.

**Fix & Measurement:** In CLI invocations (`cargo instrument -- build`), `main.rs` precomputes the session plan upfront, reducing wrapper queries to zero. For raw `RUSTC_WRAPPER` invocations, atomic file writes ensure clean plan persistence. Validated on the 105-unit parallel build fixture (`graph_scale_tests.rs`).

#### D5 - Unsafe default when `cargo metadata` fails

`SessionPlan::default()` originally set `has_otel_shim_provider: true`. If `cargo metadata` failed (e.g., malformed workspace or environment issue), the wrapper fell back to this default, falsely assuming a provider existed and attempting Tier-2 trampoline injection.

**Fix:** Flipped default to `false` and emitted a warning diagnostic upon failure to adhere strictly to S11 fail-open policy. Covered by `test_d5_metadata_failure_safe_default`.

#### D6 - R7 method-call recursion arm over-suppressed on shared method names

`is_directly_self_recursive` matched a method call on the method identifier alone, with no receiver
check. Any function named `foo` calling `anything.foo()` was classified as self-recursive and
skipped - so `fn clone()` calling `self.inner.clone()` (Arc's clone) and `fn len()` calling
`self.lock_items().len()` (Vec's len) were both dropped from instrumentation on `census-0.4.2`,
the crate used in the hero demo. The R7 note documented the conservative *path*-call false positive
(`OtherType::helper()` inside `fn helper()`) as deliberate, but the method arm was broader than
what R7 described, and it fired on the most common method names in Rust (`new`, `len`, `next`,
`clone`, `poll`, `build`, `fmt`).

**Fix:** The method arm now requires a bare `self` receiver, which needs no type resolution:
`self.foo()` counts, `self.inner.foo()` and `v.foo()` do not. The path arm is unchanged, since
distinguishing `Self::foo()` from `OtherType::foo()` does need type information the tool does not
have - that conservatism stays deliberate. Covered by
`test_r7_method_recursion_requires_self_receiver`.

**Measurement:** On `census-0.4.2`, `self_recursive` drops from 3 to 1 and eligible candidates rise
from 16 to 18. The universal reconciliation identity is unaffected in total - it moves from
$16 + 16 = 32$ to $18 + 14 = 32$. Phase 1 documents record the pre-fix split as measured at P1.8.

### Architecture Risks for Later Integration (P2.4 Opt-In Scope)

Four risks in the Tier-2 C ABI. Under the hybrid architecture ([ADR-012](decision-records.md#adr-012---hybrid-first-partydependency-instrumentation-architecture)), dependency instrumentation is the opt-in path, so these risks and their resolution are scheduled in P2.4 behind the default first-party lint-apply driver (P2.3).

R-1 and R-2 are limitations of the ABI's width and were originally scheduled as an ABI extension. R-3 and R-4 came out of maintainer review on 2026-09-10 and question whether the ABI should exist at all - see [ADR-011](decision-records.md#adr-011---the-tier-2-c-abi-is-provisional). **R-1 and R-2 are now blocked on R-4:** if `--extern` injection replaces the tier, native calls carry scope, file, line and kind for free and both risks disappear rather than being fixed.

R-3 is independent of that outcome and was resolved in P2.2 (commit [`6bf0880`](https://github.com/darkraider01/rust-compile-time-instrumentation-/commit/6bf0880)).

#### R-1: Dependency spans share hardcoded instrumentation scope

`otel-shim/src/lib.rs:119` uses `global::tracer("dependency")` as a literal string for all third-party crates, whereas Tier-1 uses `global::tracer(crate_name)`. Per-crate `InstrumentationScope` attribution is lost in Tier-2. Passing crate name across the ABI will be batched into P2.4 if the C ABI is retained.

#### R-2: File, line, and kind transmitted across ABI and discarded

`__otel_span_enter(name, name_len, _file, _file_len, _line, _kind)` in `otel-shim/src/lib.rs:87-137` leaves file, line, and kind underscore-prefixed and unused, hardcoding `SpanKind::Internal`. Semantic convention attributes (`code.function.name`, `code.file.path`, `code.line.number` per §16.14) will be wired into the span builder during P2.4 if the C ABI is retained.

#### R-3: A panic inside the shim aborts the host process

All seven exported symbols in `otel-shim/src/lib.rs` and both declarations spliced by `transform.rs`
were plain `extern "C"`, not `extern "C-unwind"`. Since Rust 1.71, a panic reaching a plain
`extern "C"` boundary aborts the process and cannot be caught by `catch_unwind` in the host
application. This was reachable in practice, not in theory: the shim already carries a fix for a
`RefCell` double-borrow panic in `__otel_span_exit` (`otel-shim/src/lib.rs:150`), so the class of
bug that reaches this path had occurred once already.

The blast radius is worse than the uninstrumented behaviour it replaces - an application that
isolates panics per request loses the whole process instead of one request - which puts it in direct
conflict with S11.

**Mitigation landed (P2.2):** Implemented both layers described in
[ADR-011](decision-records.md#adr-011---the-tier-2-c-abi-is-provisional):
1. **Layer 1 (Fail-open panic containment):** `catch_unwind` with narrow `AssertUnwindSafe` inside each of
   the seven exported functions in `otel-shim/src/lib.rs`. On panic, it swallows the error and returns the
   S9 no-op value (`0` for `u64` handles/tokens, unit for the rest). Telemetry failure never propagates into
   user code. Crucially, this prevents a double-panic abort when `__OtelGuard::drop()` calls `__otel_span_exit`
   while unwinding from an application panic.
2. **Layer 2 (ABI backstop):** Changed `extern "C"` to `extern "C-unwind"` across all nine declarations
   (the seven exports in `otel-shim` and both spliced blocks emitted by `transform.rs`). Any panic escaping
   across the boundary unwinds safely instead of triggering an immediate process abort.

**Stated limit:** `panic = "abort"` makes both layers inert. If a user's compilation profile specifies
`panic = "abort"`, unwinding never runs and the abort occurs regardless.

#### R-4: The C ABI may not be necessary at all

ADR-003 chose the C ABI because a dependency crate cannot name `opentelemetry` without an edit to
its `Cargo.toml`. Maintainer review challenged that premise: the tool already owns the full `rustc`
argv, and `--extern <name>=<path>` creates the dependency edge directly, with no manifest and no
Cargo resolution. If that holds, dependency crates emit the same native calls Tier-1 does and the
tier collapses.

**Spiked 2026-09-10 - viable.** Crate-instance identity, the unknown this risk was originally
blocked on, is not a problem: an injected `--extern` resolves to the same instance Cargo did, and a
`Context` crosses the boundary in both directions. The real blocker is build ordering - on a cold
build the dependency compiles before `opentelemetry` exists, since Cargo's DAG has no edge between
them, and injection fails with `E0433`. A pre-pass (`cargo build -p opentelemetry` into the session
target directory before the main build) resolves it on both `dev` and `release`, and Cargo reuses
the artifact rather than rebuilding it, so the cost is scheduling rather than a second compile.

**Round 2 (2026-09-10).** Cross-compilation works for free - discovery reads `-L dependency=`, which
is already target-aware. A graph where the app reaches `opentelemetry` only through `otel-shim` also
works, so the feared "pre-pass has nothing to build" case does not arise. Two `opentelemetry`
versions in one graph does break it, and nondeterministically: which rlibs exist when the dependency
compiles depends on scheduling, so the same project can build one run and fail the next with
`E0433`. Filename globbing is unsound; the fix is to capture the authoritative path from a
version-qualified pre-pass with `--message-format=json` and store it in the `SessionPlan`.

A structural limit survives that fix: a dependency is compiled once and shared, so if two binaries
resolve different `opentelemetry` versions, the single shared compilation can satisfy at most one -
the same shape as [G7](#g7---mixed-provider-workspace-fails-to-link). Still untested:
host/build-script units and `-C metadata` interaction. Full results in
[ADR-011](decision-records.md#adr-011---the-tier-2-c-abi-is-provisional).

### Coexistence with Explicit Instrumentation

Explicit developer instrumentation (`#[tracing::instrument]`, manual `tracer.start()`, and draft `#[propagate_context]`) coexists cleanly with automatic compile-time instrumentation:

1. **Precedence (S10):** Explicit instrumentation always wins at function granularity. Functions bearing span-creating attributes (`#[instrument]`, `#[instrument_span]`) or manual span creation are skipped by `ast.rs` to avoid double-instrumentation. Context-propagating attributes (`#[propagate_context]`) are also conservatively skipped in P2.2 to prevent identifier shadowing collisions (`__otel_cx`).
2. **Hybrid Parenting via `tracing-opentelemetry`:** `OpenTelemetryLayer::with_context_activation` defaults to enabled. When execution enters a `#[tracing::instrument]` span, its OpenTelemetry `Context` is automatically activated on the current thread/task. Any downstream automatically-instrumented dependency spans query `Context::current()` and automatically parent under the caller's explicit span with zero manual coordination.
3. **Three-Level Source of Truth:**
   - **Unit Level (`SessionPlan` + `CrateRole` + `UnitId`):** Determines whether an entire compilation unit is eligible for instrumentation (excluding host tools, proc-macro dependencies, and crates lacking `otel-shim`).
   - **Function Level (`DiscoveryReport.candidates` / `skipped_stats`):** Pure AST analysis in `ast.rs` that determines which individual functions are instrumented vs skipped. Preserves the reconciliation identity: `candidates.len() + skipped_stats.total() == total_functions`.
   - **Splice Level (`TransformationPlan.skipped`):** Final idempotence guard inspecting existing anchor sentinels.
4. **Safety Tradeoff (Over-Suppression):** `body_has_handwritten_otel` conservatively checks the entire function body. If an un-annotated function calls `.with_context(cx)`, automatic span creation is suppressed for that function. This trades minor span coverage for guaranteed prevention of double instrumentation and trace corruption.
5. **Upstream Alignment:** Tracking [opentelemetry-rust-contrib#791](https://github.com/open-telemetry/opentelemetry-rust-contrib/issues/791) / [PR #792](https://github.com/open-telemetry/opentelemetry-rust-contrib/pull/792) (`#[propagate_context]`). Its design is span-neutral (propagation-only), fully complementary to this tool's automatic span creation.

---

## 3. P2.1 Scope

### Responsibilities

| Responsibility | Mechanism |
|---|---|
| Compilation-unit identity | `UnitId = (crate_name, metadata_hash)` recovered from `-C metadata` in argv |
| Mirror isolation | One private mirror directory per unit |
| Mirror write safety | Temp file + atomic rename, eliminating the concurrent read/write window |
| Host-set exclusion | Packages reachable via a `build` dependency edge or from a `proc-macro` target are never instrumented |
| Link-provider gate | No Tier-2 trampoline is spliced unless `otel-shim` is reachable in the graph |
| Fail-open preservation | Every new failure path warns and compiles the original source |

### Explicit non-responsibilities

Ordering, topological sorting, parallelism, job control, freshness and feature resolution
all remain Cargo's, and P2.1 introduces no structure that duplicates them. It also does not
touch the `Emitter` trait, the AST/candidate layer, the splicing engine, or the `otel-shim` ABI.

---

## 4. Regression Suite (P2.1 Step 1 - complete)

All four tests live in
[`cargo-instrument/tests/graph_topology_tests.rs`](../../cargo-instrument/tests/graph_topology_tests.rs).
Each asserts the **correct** Phase-2 behaviour, so each is red today and flips green when
the corresponding fix lands.

| Test | Defect | Graph under test | Asserted observable |
|---|---|---|---|
| `test_multiple_versions_of_one_package_must_not_share_a_mirror` | G1 | `app → foo v1`, `app → bar → foo v2` | Instrumented output equals uninstrumented baseline (`2 3`); two mirrors exist for `foo`; no mirror holds both versions' files |
| `test_proc_macro_host_dependency_must_not_be_instrumented` | G2 | `app → pmmacro (proc-macro) → pmdep` | No mirror is produced for `pmdep`; build succeeds |
| `test_lib_and_test_units_must_not_share_a_mirror` | G3 | one package, `src/lib.rs` + `tests/it.rs` | Mirror directory count equals the number of units the wrapper reports mirroring |
| `test_no_shim_provider_must_not_emit_trampolines` | G4 | `app (bin) → dep_lib`, no `otel-shim` | No trampoline symbols spliced; build succeeds |
| `test_d1_session_cache_fingerprint_invalidation_both_directions` | D1 | `app → dep_lib`, edit `Cargo.toml` | Cache invalidates on manifest hash change; both add and remove directions transition policy correctly |
| `test_d2_hyphenated_proc_macro_host_dependency` | D2 | `app → pm-macro → pm-dep` (hyphenated) | Hyphenated package names normalized to underscores; host dependency excluded from instrumentation |
| `test_d3_cli_session_plan_avoids_wrong_manifest_heuristic` | D3 | workspace with decoy sibling manifest | CLI precomputes plan from execution dir and passes via `CARGO_INSTRUMENT_SESSION`; correct manifest policy used |
| `test_d3_cli_session_plan_respects_manifest_path_flag` | D3 | `--manifest-path` to external crate with decoy cwd | CLI parses `--manifest-path` and computes plan for target workspace rather than caller cwd |
| `test_d5_metadata_failure_safe_default` | D5 | workspace where `cargo metadata` fails | Default plan sets `has_otel_shim_provider: false`; fails open without injecting trampolines |
| `test_g7_mixed_provider_workspace_fails_open_for_common_dep` | G7 | mixed workspace: `app_a` (shim), `app_b` (no shim), shared `common` | `common` not instrumented per S11 fail-open; both binaries build and run with exit 0 |
| `test_g7_single_binary_with_shim_instruments_common_dep` | G7 | single-binary workspace: `app_a` (shim), shared `common` | `common` receives Tier-2 trampolines; `app_a` builds and runs with exit 0 |

### Design notes

- **Integration-level by necessity.** All four defects live in the interaction between
  Cargo's build graph and `run_wrapper`; none is reachable from a synthetic unit test over
  `CrateInvocation::parse` or `TransformationPlan`. Every test drives real `cargo`/`rustc`
  subprocesses over generated fixture crates, in the style of `cargo_integration_tests.rs`.
- **Offline by construction.** No fixture sets `CARGO_INSTRUMENT_REGISTRY`; every
  instrumented crate is a local path dependency. The only registry resolution performed is
  for `otel-shim`'s own `opentelemetry` dependency, which the workspace already builds.
- **Deterministic under races.** G1's outcome depends on which `rustc` process wins a race,
  so the test does not assert *which* wrong answer appears. It compares the instrumented run
  against an uninstrumented baseline of the same fixture: whichever version wins, the answer
  is wrong, so the assertion is stable.
- **Mechanism checked before effect.** Mirroring happens before `rustc` runs, so mirror-state
  assertions are placed before build-status assertions and survive a failed build.
- **Isolated defects.** The G3 fixture declares `otel-shim` so it is instrumented with the
  sentinel emitter (no C-ABI symbols, no link dependency), keeping it independent of G4.
- **Ignore-gated.** The tests are `#[ignore]`d so the default offline suite stays green -
  the same convention `e2e_registry_tests.rs` uses. This keeps `main` CI green while the
  defects are pinned.

### Running

```bash
cargo test --test graph_topology_tests -- --ignored --nocapture
```

Baseline on `409b774`: **0 passed; 4 failed.** With P2.1 and G7 landed: **11 passed; 0 failed.**

---

## 5. Definition of Done (P2.1)

1. In a diamond graph, exactly one mirror exists for the shared dependency and its span
   appears once per invocation.
2. With `foo v1` and `foo v2` in one graph, two mirrors exist and each mirror's `.rs` file
   set is byte-identical to exactly one version's source set.
3. The proc-macro fixture builds with exit code 0 and no `unresolved external symbol`.
4. A shim-less graph builds with exit code 0 and reports a skip diagnostic.
5. Three consecutive runs produce identical mirror directory names.
6. A ≥100-unit fixture completes with zero partial-file or sharing-violation failures.
7. No code path introduced by P2.1 can fail a build; the existing preflight hard-exit (G5)
   is downgraded to a warning.
8. All 145 Phase-1 tests remain green.
9. All Phase-2 regression tests pass, having been demonstrated red on `409b774`.
10. `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` clean.
11. Items 1-10 green on Linux, Windows and macOS.
12. Clean-build overhead stays within the Phase-1 band (+3.7% to +5.3%) at N=5. In CLI invocations
    (`cargo instrument -- build`), build-graph queries run at most once per build (precomputed upfront in
    `main.rs` and exported via `CARGO_INSTRUMENT_SESSION`). In raw `RUSTC_WRAPPER` invocations, atomic
    caching ensures deterministic resolution without cross-process corruption.

## 6. Phase 2 Milestone Roadmap & Implementation Sequences

### 6.1 P2.1 - Unit Identity, Instrumentation Policy & Mirror Isolation (Complete)

| Step | Work | Status |
|---|---|---|
| 1 | Regression lock-down - fixtures and tests for G1-G4 | ✅ Complete |
| 2 | `UnitId` + `-C metadata` parsing (pure, unit-testable) | ✅ Complete |
| 3 | Re-key the mirror path on `UnitId`; atomic mirror writes | ✅ Complete |
| 4 | Instrumentation policy: host-set exclusion + link-provider gate | ✅ Complete |
| 5 | Scope `remap_dep_info_files` to the unit's own `-C extra-filename` (G6) | ✅ Complete |
| 6 | Downgrade the preflight hard-exit to an S11 warning (G5) | ✅ Complete |
| 7 | Scale fixture (≥100 units) and cross-platform validation | ✅ Complete |
| Closeout | Closeout hardening: D1 (fingerprint cache), D2 (hyphen normalization), D3 (CLI session export), D4 (parallel query storm verification), D5 (fail-open metadata default) | ✅ Complete |

Steps 2-3 alone close G1 and G3.

### 6.2 P2.2 - Macro Expansion Resilience & Coexistence (Complete)

- **Attribute Matcher Widening ([ADR-009](decision-records.md#adr-009---explicit-instrumentation-wins-at-whole-function-granularity)):** Recognizes any qualified path (`#[tracing::instrument]`, `#[tracing_attributes::instrument]`, `#[otel_instrument::instrument]`, `#[propagate_context]`) and skips such functions whole to avoid double-instrumentation.
- **Identifier Collision Prevention:** Avoids `__otel_cx` name shadowing when explicit context propagation is present.
- **Hybrid Parenting Proof ([ADR-010](decision-records.md#adr-010---hybrid-parenting-is-delegated-to-tracing-opentelemetry)):** Explicit `#[tracing::instrument]` caller activates its context via `tracing-opentelemetry`, and downstream automatic dependency spans attach as children across sync and `#[async_trait]` boundaries.
- **C-ABI Panic Containment (R-3, commit [`6bf0880`](https://github.com/darkraider01/rust-compile-time-instrumentation-/commit/6bf0880)):** Implemented fail-open `catch_unwind` (Layer 1) and `extern "C-unwind"` boundary declarations (Layer 2) per [ADR-011](decision-records.md#adr-011---the-tier-2-c-abi-is-provisional).

### 6.3 P2.3 - First-Party Lint-Apply Driver (`cargo instrument-rust`) (Bounded vertical slice complete)

**Delivered slice:** `cargo instrument-rust --apply` builds an isolated nightly `rustc_driver` HIR frontend, sets it as `RUSTC`, and lets Cargo retain its `RUSTC_WRAPPER` Rustfix proxy. It applies marker-backed `MachineApplicable` body edits for ordinary free functions, inherent methods, and async functions in selected workspace packages with `opentelemetry` available. The stable workspace remains free of `rustc_private`; only the excluded driver requires nightly plus `rustc-dev`.

#### Feasibility Spike Results (commit [`0b98e5f`](https://github.com/darkraider01/rust-compile-time-instrumentation-/commit/0b98e5f), 2026-09-10)

Spiked via a standalone driver (`spikes/adr012-lint-span-probe.rs`) against a multi-shape fixture (`spikes/adr012-lint-span-fixture.rs`):

- **✅ `#[async_trait]` methods are reachable:** Proc-macro token pass-through preserves call-site spans (`body_span.from_expansion == false`, snippet `{ a + 4 }`). The outer `Box::pin(async move { .. })` wrapper is expansion, but the written body is not. The default path will not regress the `#[async_trait]` coverage proven in P2.2.
- **✅ Body wrapping is expressible:** `SourceMap::span_to_snippet(body_span)` returns the original body text, allowing `span_suggestion` to emit a prologue (`__otel_tracer`, `__otel_span`, `__otel_cx`, `_guard`) and re-emit the original inner statements.
- **❌ `macro_rules!`-generated functions are out of reach:** Correctly flagged with `from_expansion == true`. This is a real limit, but not a regression: `syn` operates pre-expansion and never saw generated items either.
- **Implementation findings:**
  1. *Async desugaring:* Emits an internal `<closure>` HIR body with `from_expansion = true` that must be skipped to avoid double-targeting.
  2. *Visitor deduplication:* Impl items are visited twice under `rustc_middle::hir::nested_filter::All`, requiring deduplication by item ID.
  3. *Windows MAX_PATH:* Linking `rustc_private` on Windows creates deep import-lib paths (~150+ chars) that exceed the 260-char limit in deep paths. In-tree driver builds require short paths or extended path prefixes.
- **Remaining verification:** Confirming actual `span_suggestion` emission and round-trip application through `cargo fix`.

#### Implementation Sequence

| Step | Work | Status |
|---|---|---|
| Spike | Body wrapping expressibility & `#[async_trait]` call-site span reachability | ✅ Complete (`0b98e5f`) |
| 1 | `rustc_private` lint driver foundation (`cargo-instrument-rust` crate) | ⬜ In Progress |
| 2 | HIR eligibility rules (port `ast.rs` rules to HIR; skip async closures; dedup impl items) | ⬜ Planned |
| 3 | `span_suggestion` transformation engine (`span_to_snippet` wrapping, `MachineApplicable`) | ⬜ Planned |
| 4 | Developer CLI surface (`--show` diagnostics preview, `--apply` clean-tree gate) | ⬜ Planned |
| 5 | `cargo fix` round-trip verification & integration regression test suite | ⬜ Planned |

#### Definition of Done (P2.3)

1. `cargo instrument-rust --show` outputs compiler diagnostics previewing span insertion for eligible functions across a crate.
2. `cargo instrument-rust --apply` writes changes directly to disk and refuses to run if the git working tree has uncommitted changes.
3. `#[async_trait]` method bodies are correctly wrapped with telemetry spans without syntax or compilation errors.
4. Modified source files compile cleanly on stable Rust with zero compile-time wrapper latency on subsequent builds.
5. Round-trip application verified through automated integration tests.

### 6.4 P2.4 - Opt-In Dependency Pipeline & Async Trampolines (Planned)

**Objective:** Harden the transparent dependency instrumentation pipeline (`RUSTC_WRAPPER`) as an explicit opt-in mode (`--with-dependencies` or `CARGO_INSTRUMENT_DEPENDENCIES=1`) for users who require zero-code telemetry across third-party crates.

#### Key Focus Areas

1. **R-4 Resolution ([ADR-011](decision-records.md#adr-011---the-tier-2-c-abi-is-provisional)):** Evaluate `--extern` injection with version-qualified pre-pass artifact capture (`--message-format=json`). If viable, collapse Tier-2 into native calls (retiring R-1 scope attribution and R-2 file/line/kind limits). If multi-version satisfiability prevents full adoption, extend the C ABI for R-1 and R-2.
2. **Async Dependency Trampolines:**
   - `tokio::spawn` context propagation via spawn-site context capture and span links ([ADR-001](../research/17-decision-records.md#adr-001--generate-native-opentelemetry-api-calls)).
   - Stream / Sink poll-boundary instrumentation.
   - Cancelled-vs-completed span status tracking across task lifecycles.
3. **Opt-In CLI Integration:** Seamless orchestration connecting first-party lint-applied crates with dependency wrapper builds.

### 6.5 P2.5 - Large Graphs & Cross-Platform Validation (Planned)

**Objective:** Validate performance, build caching, and cross-platform correctness across both hybrid modes at scale.

#### Key Focus Areas

1. **Scale Benchmarking:**
   - First-party lint-apply: analysis speed across large multi-crate workspaces.
   - Opt-in dependency wrapper: build-time overhead on ≥100-unit dependency graphs with atomic session plan caching.
2. **Tracer Caching (`OnceLock`):** Re-baseline and implement tracer caching per §16.3.
3. **Cross-Platform Verification:**
   - Windows (`x86_64-pc-windows-msvc`) with MAX_PATH mitigation.
   - Linux (`x86_64-unknown-linux-gnu`, ELF dynamic linking).
   - macOS (`aarch64-apple-darwin`, Mach-O).

---

## 7. Milestone Ordering and Phase-1 Deferrals

```text
P2.1 Unit Identity & Mirror Isolation ✅
  │
  ▼
P2.2 Macro Expansion Resilience & Coexistence ✅
  │
  ├────────────────────────────────────────────────────────┐
  ▼                                                        ▼
P2.3 First-Party Lint-Apply (Default Path) ◀── current    P2.4 Opt-In Dependency Pipeline & Async
  │                                                        │
  └───────────────────────────┬────────────────────────────┘
                              ▼
            P2.5 Large Graphs & Cross-Platform Validation
```

P2.1 is a hard prerequisite: it established unique unit identity and mirror isolation, without which multi-unit builds collided.
P2.2 proved coexistence with explicit instrumentation and demonstrated hybrid parenting across `#[async_trait]` boundaries.
P2.2 directly enabled the ADR-012 feasibility spike: confirming that `#[async_trait]` method bodies preserve call-site spans, clearing P2.3 to build the new default first-party lint-apply path without fear of coverage regression.
P2.3 is the active milestone: building the `cargo instrument-rust` lint driver, providing zero-overhead, reviewable instrumentation for first-party crates.
P2.4 follows on the opt-in track: resolving R-4 (`--extern` injection vs C ABI) and implementing async dependency trampolines.
P2.5 brings both paths together for large-scale graph benchmarking and cross-platform verification.

| Phase-1 deferral | Lands in | Rationale |
|---|---|---|
| First-party body wrapping via `span_suggestion` | **P2.3** | Core engine for the default lint-apply workflow (ADR-012) |
| HIR eligibility rules & AST reconciliation | **P2.3** | Port of `ast.rs` rules to rustc HIR with visit deduplication |
| Developer CLI (`--show`, `--apply`) | **P2.3** | Reviewable diagnostics and clean-tree in-place rewriting |
| `tokio::spawn` context propagation | **P2.4** | Needs spawn-site context capture and span links (ADR-001) for opt-in deps |
| Stream / Sink instrumentation | **P2.4** | Same poll-boundary machinery, larger surface on opt-in deps |
| Cancelled-vs-completed span status | **P2.4** | Falls out of owning the async lifecycle in dependencies |
| R-4 `--extern` injection vs C-ABI | **P2.4** | Settles whether Tier-2 collapses into native calls or requires R-1/R-2 ABI extensions |
| Tracer caching (`OnceLock`) | **P2.5** | Performance; gate on a re-baselined benchmark per §16.3 across both modes |
| Build-script / package-graph metadata (H1) | **P2.1** | This is exactly the build-graph knowledge P2.1 introduces |
| AST fallback coverage (`Result<&str, E>`) | **P2.2** | Sits with the other return-type precision work |

---

## 8. Platform Limitations

- **G2's link failure is MSVC-specific.** On ELF targets a shared object may tolerate
  undefined symbols at link time and fail at *runtime* instead - a worse outcome. The
  regression test's primary assertion is therefore the platform-independent one (no mirror is
  produced for the host-side unit), with build success as a secondary check.
- **G1's build outcome is race-dependent.** It has been observed both as a hard failure
  (`os error 32`, Windows file sharing) and as a silent miscompile with exit code 0. The
  regression test asserts the semantic outcome, which is stable on all platforms.
- **G4 reproduces identically everywhere,** because executables require full symbol resolution.
- **Windows MAX_PATH with `rustc_private` linking.** Linking against compiler-internal libraries
  (`rustc_driver`, `rustc_interface`, etc.) produces deep intermediate symbol and library names.
  During the P2.3 feasibility spike on Windows, deep paths (~150+ chars) exceeded the Win32 260-char
  `MAX_PATH` limit, requiring execution from a short root (`C:\Users\branybuck\lspike`). In-tree
  tooling and test fixtures must keep target paths compact or enable extended path syntax (`\\?\`).
- **Toolchain channel dependency.** The default lint-apply driver (P2.3) requires `rustc_private`,
  which is available on nightly toolchains (or via channel-unlock flags during development), though
  the modified code it produces compiles on stable Rust. The opt-in dependency wrapper (P2.4)
  continues to run on stable Rust.
- The regression suite has so far been executed on Windows (x86_64-pc-windows-msvc, rustc
  1.97.1). Linux and macOS confirmation is part of P2.5.
