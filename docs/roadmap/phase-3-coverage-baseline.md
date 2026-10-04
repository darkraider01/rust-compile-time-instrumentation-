# Phase 3 Baseline — Benchmark Audit, Coverage Fixture, and Trace Oracle

[Project README](../../README.md) · [Phase 3 roadmap](phase-3-evaluation.md)

Status: first Phase 3 baseline executed 2026-10-05. This note records only evidence that was actually obtained on this date. It makes no performance claim: the full overhead benchmark suite was not executed. Observed results below come from commands run as documented in [How to rerun](#how-to-rerun); items marked *read from code* were verified by inspection but not executed as experiments.

## What was evaluated

1. **Overhead benchmark audit** (`cargo-instrument/benches/bench_overhead.rs`): which workflow it actually launched, ambient-environment sensitivity, subprocess failure handling, span assertions, raw-sample retention, and telemetry costs included — followed by a focused correction that makes the experiment explicit.
2. **Small deterministic fixture** (`p31-dep` / `p31-app`) exercised through the real public dependency workflow (`cargo instrument --with-dependencies -- run`), reusing this repository's existing test infrastructure in `cargo-instrument/tests/dependency_instrumentation_e2e_tests.rs`.
3. **Static coverage expectations** defined independently from the fixture sources (not from analyzer output), then checked against wrapper transform counts, emitter-route logs, and the dependency mirror.
4. **Runtime trace oracle** asserting application outputs, span names and counts, parent-child ancestry, trace containment across two interleaved request roots, context restoration, async outcome attributes, and explicit classification of an unsupported spawn site.
5. **First-party source application** evaluated separately by re-running the existing pinned-driver apply test (see [Evidence ledger](#evidence-ledger)).

## Environment and versions

| Item | Value |
| --- | --- |
| OS / host | Windows, `x86_64-pc-windows-msvc` |
| Stable toolchain | `1.98.0` default, `rustc 1.98.1`, `cargo 1.98.1` |
| Pinned HIR toolchain | `nightly-2026-09-09` (from `tools/p23-toolchain.txt`), components installed: `rustc-dev`, `rust-src`, `llvm-tools`, `rust-std`, `cargo` |
| OpenTelemetry | `opentelemetry 0.32.0`, `opentelemetry_sdk 0.32.1` (repository-compatible versions, resolved offline from the local registry cache) |
| Fixture network mode | `CARGO_NET_OFFLINE=true`, `--offline` on all fixture Cargo commands |

## 1. Overhead benchmark audit

### Observed with probe runs

A minimal workspace fixture (excluded path dependency `probe-dep` with two sync functions, workspace member `probe-app` depending on `opentelemetry` and `otel-shim`) was built three ways on 2026-10-05. Policy markers and wrapper debug lines were captured from stderr:

| Invocation (clean environment) | Policy marker | Wrapper activity observed |
| --- | --- | --- |
| `cargo-instrument -- build` | `legacy-v1` | `probe_app`: `selecting native OpenTelemetry emitter`, `transformed 1 candidates`; `probe_dep`: `selecting native R-4 emitter`, `transformed 2 candidates` |
| `cargo-instrument --with-dependencies -- build` | `dependencies-v1` | `probe_dep` only: `selecting native R-4 emitter`, `transformed 2 candidates`; no `probe_app` wrapper lines at all |
| `cargo-instrument -- build` with ambient `CARGO_INSTRUMENT_DEPENDENCIES=1` | `dependencies-v1` | `probe_dep` only, as above |

Conclusions drawn from those observations:

- The benchmark as previously written (no `--with-dependencies`) exercised the **legacy-v1** workflow, under which both the workspace application and the excluded path dependency were instrumented. It did not exercise the documented public dependency workflow (`dependencies-v1`), even though its runtime assertions target dependency spans.
- The flagless invocation is **environment-sensitive**: an ambient `CARGO_INSTRUMENT_DEPENDENCIES` value silently changes the policy (observed directly). This is the concrete mechanism behind the roadmap's "inherits instrumentation environment settings" note.

### Read from code (not executed as experiments)

- `parse_cli_invocation` seeds `with_dependencies` from `CARGO_INSTRUMENT_DEPENDENCIES` (`main.rs`); the wrapper additionally reads `CARGO_INSTRUMENT_REGISTRY`, `CARGO_INSTRUMENT_WRAPPER_MODE`, `CARGO_INSTRUMENT_SENTINEL_MODE`, `CARGO_INSTRUMENT_NATIVE_OTEL`, `INSTRUMENT_DEBUG` (route/debug lines), and `CARGO_INSTRUMENT_SESSION`/`CARGO_INSTRUMENT_SESSION_ID` (session reuse). Fault-injection hooks `__CARGO_INSTRUMENT_FAULT_INJECT_*` are also read. All were inherited by benchmark child processes before the correction; the baseline `cargo` invocations inherited any ambient `RUSTC_WRAPPER`.
- Subprocess status handling before the correction: clean builds, repeat/incremental builds, and runtime runs were asserted; `generate-lockfile`, the two repeat-setup builds, and both release builds were only spawn-checked (`.expect()`/`.status()` without success assertion).
- `RUNTIME_RESULT` parsing used `if let Some(...)`, so a missing or malformed line was silently skipped and could produce empty sample vectors.
- Runtime assertions before the correction: the instrumented binary asserted exactly 500,000 `compute_step_*` completed spans and the baseline asserted zero (in-app, counting processor). That proves dependency calls produced spans, but it did not assert parent-child ancestry, instrumentation scope, or the emitter route.
- Telemetry included in the timed region: both binaries run `otel_shim::init()` and build an `SdkTracerProvider` with a counting `SpanProcessor` (no exporter) **before** the timed loop; the timed region is the 100,000×5-call loop only, so per-span counting-processor cost is included while provider setup and export costs are not. Raw samples: runtime per-sample values were already printed by the application (`RUNTIME_RESULT` lines); compile-time samples were only summarized (median/min/max).

### Corrections applied (focused, benchmark-only)

In `cargo-instrument/benches/bench_overhead.rs`:

1. Every instrumented invocation now passes `--with-dependencies` (the current public dependency CLI), making the workflow `dependencies-v1` explicitly.
2. `sanitize_instrument_env` removes 14 ambient variables (`CARGO_INSTRUMENT_*`, `INSTRUMENT_DEBUG`, `RUSTC_WRAPPER`, `__CARGO_INSTRUMENT_FAULT_INJECT_*`) from every child process, following the `env_remove` pattern already used by the test `cli()` helper.
3. `run_checked` asserts exit status for `generate-lockfile`, both repeat-setup builds, and both release builds; timed builds keep their existing assertions.
4. `RUNTIME_RESULT` parsing now hard-asserts instead of silently skipping.
5. The instrumented release build captures stderr, prints every `EMITTER:` route line, and **asserts** that `bench_dep` selected the native R-4 emitter and that `bench_app` produced no wrapper activity.
6. Compile-time raw samples are printed per run as `RAW sample` lines; the report footnote now states the workflow, route, timing composition (public-command wall time including the JSON pre-pass and orchestration), and what is *not* measured.

### Remaining measurement limitations (documented, not fixed)

- The full suite was **not executed** in this baseline (five-sample clean/repeat/incremental loops plus release builds are expensive and preliminary numbers are explicitly out of scope). **No timing result from this benchmark is claimed here.** `cargo bench --bench bench_overhead --no-run` compiles the corrected benchmark.
- Raw samples are printed to stdout only; they are durable only if the bench output is captured by the caller. No file output or benchmarking framework was added, by design.
- Statistics remain median/min/max (unchanged by design); no warm-up, alternation, or confidence analysis exists yet.
- The counting processor is the only telemetry cost measured; no exporter/flush/shutdown cost and no async workload are covered.
- The bench asserts span count but not ancestry/scope — ancestry is covered by the fixture trace oracle in section 4, not by the benchmark.
- The emitter-route assertion runs on the release build only; the streamed debug builds of the same fixture are not individually route-asserted.

## 2. Evaluation fixture

New test `phase3_coverage_baseline_static_expectations_and_runtime_trace_oracle` in `cargo-instrument/tests/dependency_instrumentation_e2e_tests.rs`, extending that file's existing helpers (`cli`, `success`, `snapshot`) and its public `cargo instrument --with-dependencies -- run --offline` workflow. No new test infrastructure, dependency, exporter, or framework was added.

Fixture shape:

- `p31-app` (workspace member): handwritten `SdkTracerProvider` + `InMemorySpanExporter` setup, two manual request roots, Tokio `multi_thread` runtime, one first-party helper.
- `p31-dep` (unowned path dependency, `exclude = ["dep"]`, own manifest outside workspace membership): synchronous nested chain, async functions with a `yield_now` suspension, a supported qualified `tokio::spawn` site, an intentionally unsupported bare-import spawn site, an interleaving rendezvous over `oneshot` channels, two lifecycle futures (unpolled, cancelled-after-first-poll), and two intentional exclusions (`#[inline]`, direct self-recursion).
- Interleaving is forced by a deterministic rendezvous (both roots must signal before either proceeds) — no sleeps. A `multi_thread` runtime is present, but this fixture **tests interleaving within one poll cycle, not task migration**; cross-thread polling was neither forced nor observed.

## 3. Static coverage: expected vs observed

The expectation table was written from the fixture sources before any analyzer output was observed (it is embedded as `P31_EXPECTED` in the test). Definition-level counts only; invocation counts are separate (section 4). One `p31_dep` lib Cargo unit, one `p31_app` bin Cargo unit.

| Function | Ownership | Form | Expected | Workload exercises it | Expected runtime spans | Observed |
| --- | --- | --- | --- | --- | --- | --- |
| `leaf` | dependency | sync | instrumented | yes | 7 (6 in-tree + 1 orphan) | 7 ✓ |
| `mid` | dependency | sync | instrumented | yes | 2 | 2 ✓ |
| `outer` | dependency | sync | instrumented | yes | 2 | 2 ✓ |
| `async_chain` | dependency | async | instrumented | yes | 1 | 1 ✓ |
| `suspended` | dependency | async | instrumented | yes | 2 | 2 ✓ |
| `spawn_child` | dependency | async + supported spawn site | instrumented | yes | 1 | 1 ✓ |
| `unsupported_spawn` | dependency | async (bare spawn inside) | instrumented | yes | 1 | 1 ✓ |
| `rendezvous` | dependency | async | instrumented | yes | 2 | 2 ✓ |
| `cancelled_work` | dependency | async | instrumented | yes | 1 | 1 ✓ |
| `never_polled` | dependency | async | instrumented | created, never polled | 0 | 0 ✓ |
| `inlined` | dependency | sync | excluded: `#[inline]` | yes (called) | 0 | 0 ✓ |
| `fib` | dependency | sync | excluded: direct self-recursion | yes (called) | 0 | 0 ✓ |
| `app_helper` | application | sync | excluded: first-party source outside the dependency workflow | yes (called) | 0 | 0 ✓ |
| `request_a`, `request_b` | application | manual roots (explicit instrumentation) | explicit, no duplicates | yes | 1 each | 1 each ✓ |

Observed static evidence (from one public `cargo instrument --with-dependencies -- run --offline` run):

- Wrapper transform line for the `p31_dep` unit reported `transformed 10 candidates` — equal to the independent table's instrumented count (10), on every occurrence of that line.
- No `crate=p31_app] transformed` and no `crate=p31_app] selecting` lines: the workspace application received no wrapper instrumentation.
- Emitter route: `crate=p31_dep] selecting native R-4 emitter`; no `selecting Tier-2` anywhere — the fixture ran on the **native route**, not the C-ABI fallback.
- Dependency mirror (located via the wrapper's `into mirror <path>` log line, searched recursively): contains `span_builder(&__otel_tracer, "<name>")` for all 10 instrumented names, and none for `inlined`/`fib`.
- Supported spawn rewritten to `tokio::spawn(opentelemetry::trace::FutureExt::with_context(`; the bare-import site remains verbatim (`bare_spawn(async { leaf(30) })`, no `bare_spawn(opentelemetry...`).
- Original fixture inputs unchanged after the run (`snapshot` equality).

## 4. Runtime trace oracle: expected vs observed

Assertions live inside the application (it owns the exporter), using the repository's existing `InMemorySpanExporter` + `SdkTracerProvider` (simple exporter) convention. All of the following were **observed passing** in the run above:

**Application outputs** (deterministic): `outer(10)=24`, `app_helper(1)=4`, `async_chain(5)=6`, `spawn_child().await=22`, `unsupported_spawn().await=31`, rendezvous results `8` and `8`, `outer(3)=10`, `inlined(3)=252`, `fib(10)=55`.

**Span inventory**: 21 finished spans total, per the expected table above; every non-root span carries scope `p31_dep`, both roots carry scope `app_root`, and no span carries scope `p31_app` (no automatic first-party spans, no duplicates of the manual roots).

**Expected parent-child relationships** (each found exactly once):

| Trace root | Chain |
| --- | --- |
| `request_a` | → `outer` → `mid` → `leaf` |
| `request_a` | → `async_chain` → `suspended` → `leaf` |
| `request_a` | → `spawn_child` → `suspended` → `leaf` (supported spawn propagation) |
| `request_a` | → `unsupported_spawn` |
| `request_a` | → `rendezvous` → `leaf` |
| `request_a` | → `cancelled_work` |
| `request_b` | → `outer` → `mid` → `leaf` |
| `request_b` | → `rendezvous` → `leaf` |

- Every span was walked to the end of its parent chain and required to terminate at its **own** request root; both roots' traces are distinct and valid. The two interleaved roots never acquired each other's descendants, and all in-tree descendants stayed in their intended trace.
- **Context restoration**: after the cancelled future's first (Pending) poll returned, the current context was the caller's `request_a` span; after both branches completed, the ambient context held no span (no leaked attach guard).
- **Classification of the unsupported site**: exactly one unparented non-root span exists — the `leaf` inside the bare-import spawned task — in a third trace distinct from both request traces. This is the expected consequence of an unwritten spawn site and is asserted explicitly rather than tolerated silently.

**Async lifecycle metadata** (native dependency route, async spans only): `async_chain` 1 × `completed`, `suspended` 2 × `completed`, `spawn_child` 1 × `completed`, `unsupported_spawn` 1 × `completed`, `rendezvous` 2 × `completed`, `cancelled_work` 1 × `cancelled`. Unpolled `never_polled` produced no span. Sync spans were not required to carry outcome attributes, and no first-party span was required to carry dependency lifecycle metadata.

## Evidence ledger

| Evidence | Status on 2026-10-05 |
| --- | --- |
| `cargo test --test dependency_instrumentation_e2e_tests phase3_coverage_baseline -- --nocapture` | **Newly executed — passed** (1 passed, ~19 s) |
| Full `dependency_instrumentation_e2e_tests` file re-run (regression check) | Newly executed — see [Run results](#run-results) |
| `cargo test --test source_instrumentation_apply_tests -- --ignored` (first-party apply, pinned nightly) | Reused existing test, newly executed — see [Run results](#run-results) |
| `cargo bench --bench bench_overhead --no-run` | Newly executed — compiles, exit 0 |
| `cargo bench --bench bench_overhead` (full suite) | **Not executed** — no timing results claimed |
| Probe runs (`legacy-v1` vs `dependencies-v1` vs ambient flip) | Newly executed — section 1 |
| Existing test evidence reused by reference (not rerun): `native_otel_tests`, `tokio_spawn_tests`, `public_tier2_fallback...`, `scale_incremental_e2e_tests` | Existing, not rerun in this baseline |

### Run results

| Command | Result |
| --- | --- |
| `cargo test --test dependency_instrumentation_e2e_tests phase3_coverage_baseline -- --nocapture` | `test result: ok. 1 passed; 0 failed` (~19 s warm) |
| `cargo test --test dependency_instrumentation_e2e_tests` (whole file, regression check) | `test result: ok. 6 passed; 0 failed` (67.34 s) — the new baseline plus the five pre-existing dependency e2e cases (policy transitions, Tier-2 fallback metadata, lifecycle/build cycles, tokio metadata identity, adapter exclusions) |
| `cargo test --test source_instrumentation_apply_tests -- --ignored --nocapture` | `test result: ok. 1 passed; 0 failed` (82.63 s). Observed: the `cargo-instrument-rust-driver` compiled under the pinned `nightly-2026-09-09` + `rustc-dev`; `cargo instrument-rust --apply` reported `Fixed app\src\lib.rs (29 fixes)` and committed `apply P2.3 instrumentation` (254 insertions) in the fixture's own Git repository; the test then ran the fixture's stable check **and full stable test suite**, which includes `test_runtime_telemetry_and_result_status` — first-party runtime assertions for span parent-child/trace agreement, exclusion of eligible-but-excluded functions, and preservation of an existing `#[tracing::instrument]` span without a duplicate automatic span. Dirty-worktree refusal and idempotent second apply were also asserted. |
| `cargo bench --bench bench_overhead --no-run` | compiles, exit 0 |
| `cargo clippy -p cargo-instrument --all-targets -- -D warnings` | exit 0 |
| `cargo fmt -p cargo-instrument -- --check` | exit 0 |
| Probe runs (section 1) | all three invocations exited 0 |

First-party, dependency, and fallback claims are kept separate: the first-party line above covers only source application of the pinned HIR driver (its own stable runtime test), the fixture sections cover only the native dependency route, and fallback behavior was not re-evaluated in this baseline.

## Limitations and defects

- **No product defect was found** in this baseline. Two audit gaps were found *in the benchmark script* and corrected (unspecified workflow; unchecked setup subprocesses). One test-side misunderstanding was resolved: the dependency mirror stores files relative to the compiler's working directory (workspace root), so `p31_dep`'s source appears at `<unit>/dep/src/lib.rs`, not `<unit>/src/lib.rs`; the test locates `lib.rs` recursively instead of assuming a layout.
- First-party and dependency evidence are kept separate: the fixture runs only the dependency workflow; first-party coverage comes from the separate apply test (which has its own generated-code runtime assertions). No first-party span was used as evidence for dependency behavior, or vice versa.
- The fixture proves interleaving, not cross-thread migration (see section 2). It proves the native route for this fixture; fallback-route behavior remains covered by the pre-existing `public_tier2_fallback...` test, not by this baseline.
- Static expectations were hand-derived from a 12-function dependency; analyzer output is used only as the observed cross-check, never as the denominator.
- Single machine, single run, Windows host; no repetitions and no statistical treatment anywhere in this note.

## How to rerun the baseline

```sh
# Coverage + trace oracle fixture (public dependency workflow; ~20 s warm):
cargo test --test dependency_instrumentation_e2e_tests phase3_coverage_baseline -- --nocapture

# Whole file including the pre-existing dependency e2e cases:
cargo test --test dependency_instrumentation_e2e_tests

# First-party apply evidence (requires the pinned toolchain from tools/p23-toolchain.txt):
cargo test --test source_instrumentation_apply_tests -- --ignored --nocapture

# Benchmark: compile-check only (cheap), or the full expensive suite:
cargo bench --bench bench_overhead --no-run
cargo bench --bench bench_overhead
```

Record with any rerun: `rustc -vV`, `cargo -V`, `rustup show` for the pinned nightly, the git revision, and the captured stdout (the benchmark prints `RAW sample` and `EMITTER:` lines that constitute its retained raw evidence).
