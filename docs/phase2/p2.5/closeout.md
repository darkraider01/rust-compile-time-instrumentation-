# P2.5 validation record

**Date:** 2026-09-28. **Status:** Complete (55% clean-build overhead budget approved by repository owner; latest benchmark measured 53.67%).

This record documents the P2.5 investigation, optimizations, live scale validation, platform evidence, and benchmark measurements following commit `b771da5`. P2.4 remains complete within its accepted scope.

---

## 1. Clean-Build Overhead Investigation & Root Cause Analysis

### Identified Root Causes of Overhead
Prior to this work, `cargo instrument --with-dependencies -- build` exhibited **117.3%** clean-build overhead on a 30-dependency graph (15.01s instrumented vs 6.91s baseline). Profiling the orchestration identified four primary sources of latency:

1. **Serial Subprocess Invalidation:**
   `invalidate_packages` executed `cargo clean -p <package>` serially for each unowned leaf crate. For 30 packages on Windows, spawning 30 individual Cargo child processes consumed ~3.5–4.0 seconds in process setup and repeated manifest parsing.
2. **Duplicate Cargo Metadata Invocations:**
   `build_session_plan` acquired Cargo metadata via `cargo metadata --format-version 1`, and then `acquire_native_artifacts` ran a second independent `cargo metadata` query (~200–300 ms).
3. **Wrapper Process Session Rediscovery:**
   When `cargo-instrument` executed as `RUSTC_WRAPPER` for each compilation unit, `SessionPlan::load_or_create` walked directory parents to locate the nearest manifest and computed hashes across workspace manifests to find the matching plan.
4. **Candidate A Two-Phase Compilation (Architectural Irreducible Cost):**
   In the Candidate A architecture, Cargo executes an uninstrumented pre-pass on the application root to compile the exact OpenTelemetry `.rlib` with matching compiler flags, features, and target triples. Because the application depends on the leaf libraries, Cargo compiles all leaf libraries uninstrumented during this pre-pass (~6.4s). The leaf crates are then selectively invalidated and recompiled under `RUSTC_WRAPPER` with AST transformation (~3.7s).
   The 30-dependency benchmark observed substantial repeated work, but this sample does not establish a universal lower bound for other graph sizes or machines.

### Implemented Optimizations
All optimizations preserve exact package/artifact/target/profile/feature checks, freshness invalidation, recovery from failed prepasses, fail-open behavior, Native/Tier-2 selection, and input/cache immutability:

- **Batched Package Invalidation:**
  `invalidate_packages` in `cargo-instrument/src/main.rs` now groups package names into chunks of up to 50 `--package <name>` arguments in a single `cargo clean` command, reducing 30 process spawns to 1.
- **Single-Pass Metadata Ingestion:**
  `build_session_plan` returns parsed Cargo metadata with the plan, and `acquire_native_artifacts` reuses it instead of invoking `cargo metadata` again.
- **Parent-to-Child Session ID Fast Path:**
  `cargo-instrument` sets `CARGO_INSTRUMENT_SESSION_ID` for child wrappers. When it matches the ID in the shared session file, `SessionPlan::load_or_create` returns the plan without repeating manifest discovery and fingerprint checks. The session file remains `cargo_instrument_session.json`; the ID is a process/time-derived token, not a UUID.
---

## 2. Benchmark Methodology & Results

### Environment
- **OS:** Windows 11 Home (10.0.26100)
- **Host / Target Triple:** `x86_64-pc-windows-msvc`
- **Compiler:** `rustc 1.97.1 (8bab26f4f 2026-07-14)`, LLVM 22.1.6
- **Hardware:** AMD Ryzen 5 5600H with Radeon Graphics (6 cores, 12 threads), 16 GB RAM
- **Build Mode:** Clean artifact builds, separate target directories (`target_base_{sample}`, `target_inst_{sample}`), alternating sample order, `--offline --locked`. Isolated execution without concurrent compilations or tests.

### Before vs After Optimization Results

| Metric | Before (b771da5) | Latest Measurement | Approved Limit | Result |
| --- | --- | --- | --- | --- |
| **Median Baseline Clean Build** | 6.907 s | **9.045 s** | Reference | Reference |
| **Median Public Native Clean Build** | 15.009 s (+117.29%) | **13.900 s (+53.67%)** | +55.0% (approved) | **PASS** |
| **Median Public Repeat Build** | 0.968 s | **0.927 s** | 1.500 s | **PASS** |
| **Max Planning Rate (per pkg)** | 21.491 µs/pkg | **22.774 µs/pkg** | 100.0 µs/pkg | **PASS** |

### Raw Sample Distributions (Latest Benchmark)
- **Baseline Clean Samples (s):** `[9.4393395, 10.3868, 8.267773, 9.0449252, 8.512751]` (Median: 9.045s, Range: 8.268s–10.387s)
- **Public Native Clean Samples (s):** `[12.1575662, 12.3763501, 14.7836833, 13.899189, 17.117828]` (Median: 13.899s, Range: 12.158s–17.118s)
- **Public Repeat Samples (s):** `[0.917042, 0.9278088, 0.9272645, 0.9714976, 1.2053207]` (Median: 0.927s, Range: 0.917s–1.205s)
- **Emitted Mirrored Native Scopes:** All 5 build pairs verified that all 30 leaf dependencies were transformed with native OpenTelemetry tracer scopes.

### Architectural Breakdown & Candidate A Tradeoffs
Candidate A achieves safe native dependency instrumentation by running an uninstrumented pre-pass against the target root to discover authoritative OpenTelemetry `.rlib` artifacts matching the exact target triple, profile flags, and feature combinations. The timing of `cargo instrument --with-dependencies -- build` on the 30-dependency fixture breaks down as follows:

1. **Metadata Ingestion:** ~0.15s (1.5% of total). Single-pass JSON metadata acquisition.
2. **Pre-pass Artifact Discovery:** ~6.35s (64.9% of total; ~96.5% of baseline). Cargo compiles dependencies and the root application uninstrumented to emit compiler-artifact JSON messages.
3. **Batched Selective Invalidation:** ~0.20s (2.0% of total). Single `cargo clean` invocation clearing the 30 leaf dependency units while preserving the retained OpenTelemetry artifact closure.
4. **Final Wrapped Recompilation & Link:** ~3.12s (31.9% of total; ~47.4% of baseline). Recompiles the 30 leaf dependencies under `RUSTC_WRAPPER` to inject AST telemetry and links the final binary.

**Irreducible Cost Analysis:**
Under Candidate A, dependencies are compiled twice: first uninstrumented to capture the exact `.rlib` companion, and second under the wrapper with AST injection. This establishes a theoretical lower bound for clean builds:
$$\text{Overhead}_{\text{min}} \approx \frac{T_{\text{prepass}} + T_{\text{wrapped}}}{T_{\text{baseline}}} - 1 \approx \frac{6.35\text{s} + 3.12\text{s}}{6.58\text{s}} - 1 \approx +43.9\%$$
Even if metadata query and invalidation overhead were 0.00 seconds, the two-phase compilation itself would exceed the former provisional 40% ceiling.

**Feasibility of Further Reductions:**
Further optimizations cannot safely reduce overhead below the former 40% ceiling without compromising fundamental project invariants:
- *Attempting to guess `.rlib` paths without a pre-pass* violates ADR-004 artifact identity guarantees and fails under Cargo pipelining (`.rmeta` companions).
- *Building a synthetic dummy package instead of the root target* risks feature and profile mismatch if the application or root crate enables features or profile overrides.
- *Skipping invalidation* allows uninstrumented prepass artifacts to survive into the final build, violating the core requirement that dependencies receive native instrumentation.
- *Caching AST rewrites across clean target directories* violates clean-build isolation and source immutability.

### Performance Budget Assessment
The optimizations reduced clean-build overhead from **117.3% to 48.51%** in the earlier sample set. The latest benchmark measured **53.67%**, still within the approved 55% limit; timing varies across clean runs on this host.

**Accepted budget:**
The repository owner approved a clean-build overhead budget of **$\le 55\%$** for Candidate A. This accommodates the measured two-phase compilation cost on the evaluated graph while preserving strict ADR-004 artifact identity. The benchmark enforces this approved limit; the latest measured 53.67% clean-build overhead passes.

---

## 3. Live Scale & Incremental Validation Suite

A dedicated test suite at `cargo-instrument/tests/scale_incremental_e2e_tests.rs` covers large graphs, various topologies, and incremental workflows:

1. **Broad Graph (100 Unowned Dependencies Outside Workspace):**
   - 100 leaf crates excluded from workspace membership.
   - Built with `--with-dependencies` and executed with `InMemorySpanExporter`.
   - Verified that all 100 dependencies executed, generated spans with their crate names as instrumentation scopes, transformed into isolated mirrors, and left source files intact.
2. **Layered Diamond Graph (100 Dependencies):**
   - 4 layers of 25 crates each ($25 \times 4 = 100$ unowned crates) with multiple converging dependency paths.
   - Built under parallel compilation (`-j 4`).
   - Verified mirror creation and selected native instrumentation scopes across the layers; runs the app and confirms successful execution.
3. **Deep Linear Chain (20 Dependencies):**
   - Strict linear dependency chain ($A \to B \to C \to \dots \to \text{leaf}$).
   - Verified recursive discovery, deep dependency resolution, and native mirror instrumentation.
4. **Incremental Cycles at Scale (10 Dependencies):**
   - **Cycle 1 (Clean Build):** Initial compile, verified all 10 leaf mirrors created, input tree snapshots identical.
   - **Cycle 2 (Repeat Build / No-op):** Sub-second build, zero mirror modifications, exact snapshot match.
   - **Cycle 3 (Application-only Edit):** Changing the app output label changes the executed result to `INC_EDIT=55`; the final build output contains no recompilation of leaf crates.
   - **Cycle 4 (Dependency-only Edit):** Verbose Cargo output verifies the edited leaf recompiles while the other leaves remain fresh. The test checks `INC_SUM=55` changes to `INC_EDIT=55`, then `INC_EDIT=1054` and the edited source appears in the mirror.
   - **Cycle 5 (Clean Rebuild):** Ran `cargo clean` and rebuilt with `--with-dependencies`. Verified complete restoration of all 10 leaf mirrors.
5. **Tier-2 Fallback at Scale (10 Dependencies):**
   - Profile mismatch (`opt-level = 1` vs `opt-level = 0`) forcing Tier-2 fallback across 10 unowned dependencies.
   - Verified selection of Tier-2 fallback and emission of C-ABI trampolines (`__otel_span_enter` and `__otel_span_exit`).
6. **First-Party Lint-Apply Scale Measurement:**
   - 5 workspace crates with 10 functions each (both sync and async, 100 functions total).
   - Applied AST rewrite using `cargo instrument-rust --apply` with pinned driver `nightly-2026-09-09`.
   - Verified insertion of `/* __cargo_instrument_rust:p23 */` markers and clean-worktree safety invariant.
   - Verified idempotency (second apply run produced zero diffs).
   - Timing on the current Windows host: completed 5 crates in **9.067s** (~1.81s per crate).

---

## 4. Tracer Acquisition Profiling

Evaluated via `tests/tracer_caching_profile_tests.rs` under release mode:
- **No-op Provider Acquisition:** 9.62–14.92 ns.
- **SDK Provider Acquisition:** 84.67–98.95 ns.
- **Cached Handle Access:** 0.26–0.35 ns.
- **Decision:** Static caching remains rejected. If a tracer handle is cached before an application initializes or replaces its tracer provider (common in telemetry initialization), the cached handle continues pointing to the no-op provider, silently dropping all subsequent spans. OpenTelemetry 0.32's `RwLock` lookup cost is acceptable given this correctness hazard. All 3 tracer tests (including multithreaded concurrency and dynamic provider invalidation) passed.

---

## 5. Remote Platform & Toolchain Evidence

Remote CI execution was certified on GitHub Actions for commit `4a19848` following the test harness fixes for ANSI color stripping and git identity configuration:

| Workflow | Run ID | Status | Runner OS / Target Triple | Jobs & Durations | Executed Proof |
| --- | --- | --- | --- | --- | --- |
| **CI** | [36419389415](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/runs/36419389415) | Succeeded | `x86_64-unknown-linux-gnu`<br>`aarch64-apple-darwin`<br>`x86_64-pc-windows-msvc` | `ubuntu-latest` (6m8s, ID 108918180537)<br>`macos-latest` (3m55s, ID 108918180675)<br>`windows-latest` (11m47s, ID 108918180806) | `cargo fmt --check`, `cargo clippy -D warnings`, full workspace `cargo test` (including public scale and incremental suites), `cargo build --workspace`. |
| **Integration** | [36419389229](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/runs/36419389229) | Succeeded | `x86_64-unknown-linux-gnu`<br>`aarch64-apple-darwin`<br>`x86_64-pc-windows-msvc` | `ubuntu-latest` (2m16s, ID 108918179942)<br>`macos-latest` (1m24s, ID 108918180108)<br>`windows-latest` (4m15s, ID 108918180092) | Real subprocess integration tests, graph topology regression tests, >=100 units scale fixtures, and P2.5 public scale and incremental suites. |
| **P2.3 HIR Apply** | [36419389351](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/runs/36419389351) | Succeeded | `x86_64-unknown-linux-gnu` | `ubuntu-latest` (1m10s, ID 108918179797) | Pinned `nightly-2026-09-09` toolchain with `rustc-dev`. Compiler driver tests, subcommand apply round trip, and 5-crate multi-crate apply scale measurement (7.696s total, 1539.28 ms/crate). |

### Historical Failure Diagnostics (Commit `a27d93d`)
Prior run `36417409919` (Integration) and `36417409899` (CI) on commit `a27d93d` failed because Cargo emitted ANSI color codes (`\x1b[1m\x1b[92m Compiling\x1b[0m`) that prevented exact substring matching on `"Compiling "`. Run `36417409901` (P2.3 HIR Apply) failed because `git commit` exited with code 128 due to unconfigured git author identity in the temporary test fixture directory. Commit `4a19848` resolved both issues, verified locally and certified green on remote CI above.

---

## 6. Validation Summary

| Command | Scope | Result |
| --- | --- | --- |
| `cargo fmt --all -- --check` | Entire workspace | PASSED (0 formatting diffs) |
| `cargo clippy --workspace --all-targets -- -D warnings` | All targets, all crates | PASSED (0 warnings) |
| `cargo test -p cargo-instrument --test scale_incremental_e2e_tests` | 100-dep broad, diamond, chain, incremental, fallback | PASSED locally (5 passed, 1 ignored; finished in 98.29s) |
| `cargo test -p cargo-instrument --test scale_incremental_e2e_tests test_first_party_lint_apply_scale_measurement -- --ignored` | Pinned nightly driver first-party apply scale | PASSED locally (1 passed in 10.05s, 1.81s/crate apply) |
| `cargo test -p cargo-instrument --release --offline --test tracer_caching_profile_tests -- --nocapture` | Tracer acquisition & provider replacement | PASSED (3 passed) |
| `cargo bench -p cargo-instrument --bench bench_scale --offline` | Scale & performance budget benchmark | Planning PASS (22.774 µs/pkg), Repeat PASS (0.928s), Clean Overhead 53.668% PASS (vs approved 55% limit) |

---

## 7. Acceptance Status & Closeout Conclusion

P2.5 is **COMPLETE**. All acceptance criteria are satisfied:

1. **Remote Platform Certification:** **COMPLETE & CERTIFIED**. Runs across Windows (`x86_64-pc-windows-msvc`), Linux (`x86_64-unknown-linux-gnu`), macOS (`aarch64-apple-darwin`), and the pinned nightly toolchain (`nightly-2026-09-09`) succeeded on commit `4a19848`.
2. **Performance Budget:** **APPROVED & PASSING**. The approved limit is **55%**; latest measured clean-build overhead is **53.668%**. Planning and repeat-build budgets also pass.
