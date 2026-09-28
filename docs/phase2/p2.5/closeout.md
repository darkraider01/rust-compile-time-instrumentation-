# P2.5 validation record

**Date:** 2026-09-28. **Status:** In progress (clean-build budget pending review).

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

| Metric | Before (b771da5) | After Optimization | Provisional Limit | Result |
| --- | --- | --- | --- | --- |
| **Median Baseline Clean Build** | 6.907 s | **6.581 s** | Reference | Reference |
| **Median Public Native Clean Build** | 15.009 s (+117.29%) | **9.773 s (+48.51%)** | +40.0% | **FAIL (Exceeds 40%)** |
| **Median Public Repeat Build** | 0.968 s | **0.590 s** | 1.500 s | **PASS** |
| **Max Planning Rate (per pkg)** | 21.491 µs/pkg | **31.245 µs/pkg** | 100.0 µs/pkg | **PASS** |

### Raw Sample Distributions (Post-Optimization)
- **Baseline Clean Samples (s):** `[6.5806907, 6.1726653, 6.8090551, 6.12865, 6.6281724]` (Median: 6.581s, Range: 6.129s–6.809s)
- **Public Native Clean Samples (s):** `[11.2645496, 9.5969758, 9.7726675, 9.5670241, 10.6462251]` (Median: 9.773s, Range: 9.567s–11.265s)
- **Public Repeat Samples (s):** `[0.5546026, 0.5898927, 0.5799599, 0.6188045, 0.6064298]` (Median: 0.590s, Range: 0.555s–0.619s)
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
Even if metadata query and invalidation overhead were 0.00 seconds, the two-phase compilation itself exceeds the provisional 40% ceiling.

**Feasibility of Further Reductions:**
Further optimizations cannot safely breach the 40% ceiling without compromising fundamental project invariants:
- *Attempting to guess `.rlib` paths without a pre-pass* violates ADR-004 artifact identity guarantees and fails under Cargo pipelining (`.rmeta` companions).
- *Building a synthetic dummy package instead of the root target* risks feature and profile mismatch if the application or root crate enables features or profile overrides.
- *Skipping invalidation* allows uninstrumented prepass artifacts to survive into the final build, violating the core requirement that dependencies receive native instrumentation.
- *Caching AST rewrites across clean target directories* violates clean-build isolation and source immutability.

### Performance Budget Assessment & Recommendation
The optimizations reduced clean-build overhead from **117.3% down to 48.51%** (a 5.24-second / 34.9% wall-clock reduction).

**Recommendation:**
1. A budget of **$\le 55\%$** clean-build overhead is proposed for Candidate A, reflecting the irreducible two-phase compilation cost on large graphs while maintaining strict ADR-004 artifact identity.
2. In accordance with project instructions, the provisional limit of 40% was **not** artificially raised in code, and `cargo bench --bench bench_scale` exited with code 1. Acceptance of this item remains pending until the proposed 55% budget is formally approved by the repository owner.

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

Remote CI execution on GitHub Actions was inspected for the `main` branch at commit `a27d93d`:

| Workflow | Run ID | Status | Platforms / Jobs Observed | Failure Root Cause & Resolution |
| --- | --- | --- | --- | --- |
| **Integration** | [36417409919](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/runs/36417409919) | Failed | `windows-latest` (4m3s), `macos-latest` (1m56s), `ubuntu-latest` (1m52s). Ran unit/topology tests (PASS), failed on `Run P2.5 public scale and incremental tests`. | **Cause:** `compiled_packages` string parser did not strip ANSI terminal color codes emitted on CI runners (`\x1b[1m\x1b[92m Compiling\x1b[0m`), failing to match the `"Compiling "` prefix.<br>**Fix:** Implemented ANSI escape sequence stripping in `compiled_packages`, set `CARGO_TERM_COLOR = "never"` in test CLI harness, and passed `--color never`. Verified locally (5/5 passed in 98.29s). |
| **P2.3 HIR Apply** | [36417409901](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/runs/36417409901) | Failed | `ubuntu-latest` (1m12s). Ran on pinned `nightly-2026-09-09`. Failed on `Measure first-party apply across multiple crates`. | **Cause:** `git commit` exited with code 128 (`Author identity unknown`) because the temporary test fixture directory lacked git author configuration on the GitHub Actions runner.<br>**Fix:** Added explicit `git config user.name` / `user.email` and `-c user.name=... -c user.email=...` to all `git commit` commands in the test harness. Verified locally on pinned nightly (1 passed in 10.05s). |
| **CI** | [36417409899](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/runs/36417409899) | Failed | `ubuntu-latest` (5m34s), `macos-latest` (3m18s), `windows-latest` (9m18s). Ran `cargo fmt` (PASS), `cargo clippy` (PASS), failed on `Run tests` (`cargo test --workspace`). | **Cause:** Failed in `scale_incremental_e2e_tests.rs` due to the same ANSI escape sequence parsing issue as in the Integration workflow.<br>**Fix:** Resolved by the same test fixture update. |

Prior green runs for baseline commit `b771da5` ([CI 36386894186](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/runs/36386894186), [Integration 36386894301](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/runs/36386894301), [P2.3 Apply 36386894178](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/runs/36386894178)) verified the workspace prior to wiring `scale_incremental_e2e_tests`. Remote certification of the latest revision containing the test fixture fixes is pending commit and push.

---

## 6. Local Workspace Validation Summary

| Command | Scope | Result |
| --- | --- | --- |
| `cargo fmt --all -- --check` | Entire workspace | PASSED (0 formatting diffs) |
| `cargo clippy --workspace --all-targets -- -D warnings` | All targets, all crates | PASSED (0 warnings) |
| `cargo test -p cargo-instrument --test scale_incremental_e2e_tests` | 100-dep broad, diamond, chain, incremental, fallback | PASSED locally (5 passed, 1 ignored; finished in 98.29s) |
| `cargo test -p cargo-instrument --test scale_incremental_e2e_tests test_first_party_lint_apply_scale_measurement -- --ignored` | Pinned nightly driver first-party apply scale | PASSED locally (1 passed in 10.05s, 1.81s/crate apply) |
| `cargo test -p cargo-instrument --release --offline --test tracer_caching_profile_tests -- --nocapture` | Tracer acquisition & provider replacement | PASSED (3 passed) |
| `cargo bench -p cargo-instrument --bench bench_scale` | Scale & performance budget benchmark | Planning PASS (31.25 µs/pkg), Repeat PASS (0.590s), Clean Overhead 48.51% FAIL (vs 40% provisional limit) |

---

## 7. Acceptance Status & Closeout Conclusion

P2.5 and Phase 2 remain **OPEN** pending the following two items:

1. **Performance Budget Approval:** Clean build overhead is measured at **48.51%** against the provisional 40% limit. Due to the irreducible cost of Candidate A's two-phase compilation (~43.9% theoretical floor on this topology), a revised budget of $\le 55\%$ is proposed. The provisional 40% threshold is maintained in code until explicit owner approval is granted.
2. **Remote Platform Certification:** Commit `a27d93d` failed remotely due to ANSI color codes and git author identity in the test fixture harness. The fixes have been implemented and verified locally; remote certification across Windows, Linux, macOS, and pinned nightly is pending a pushed revision.