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
   Therefore, any clean build under Candidate A compiles unowned dependencies **twice** ($6.4\text{s} + 3.7\text{s} \approx 10.1\text{s}$ vs $6.6\text{s}$ baseline). Even with instantaneous invalidation, zero AST parsing overhead, and zero wrapper overhead, compiling dependencies twice establishes an architectural lower bound of $+45\%\text{--}55\%$ clean-build overhead.

### Implemented Optimizations
All optimizations preserve exact package/artifact/target/profile/feature checks, freshness invalidation, recovery from failed prepasses, fail-open behavior, Native/Tier-2 selection, and input/cache immutability:

- **Batched Package Invalidation:**
  `invalidate_packages` in `cargo-instrument/src/main.rs` now groups package names into chunks of up to 50 `--package <name>` arguments in a single `cargo clean` command, reducing 30 process spawns to 1.
- **Single-Pass Metadata Ingestion:**
  `acquire_native_artifacts` now accepts `Option<&serde_json::Value>`, reusing the metadata already parsed during session planning.
- **Parent-to-Child Session ID Fast Path:**
  `cargo-instrument` now exports `CARGO_INSTRUMENT_SESSION_ID` containing the active session UUID. Child wrapper processes check this environment variable and load the session file directly via `target_dir.join(format!(".cargo-instrument-session-{session_id}.json"))`, completely bypassing directory walking and manifest hashing.
- **Compact Session Serialization:**
  `SessionPlan::save_to_file` now uses compact `serde_json::to_vec` instead of pretty-printed JSON, speeding up serialization and deserialization across wrapper invocations.

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
| **Median Baseline Clean Build** | 6.907 s | 6.608 s | Reference | Reference |
| **Median Public Native Clean Build** | 15.009 s (+117.29%) | **9.816 s (+48.54%)** | +40.0% | **FAIL (Exceeds 40%)** |
| **Median Public Repeat Build** | 0.968 s | **0.613 s** | 1.500 s | **PASS** |
| **Max Planning Rate (per pkg)** | 21.491 µs/pkg | **21.617 µs/pkg** | 100.0 µs/pkg | **PASS** |

### Raw Sample Distributions (Post-Optimization)
- **Baseline Clean Samples (s):** `[6.608176, 6.8727987, 6.7605274, 6.3743951, 6.4614521]` (Median: 6.608s, Range: 6.374s–6.873s)
- **Public Native Clean Samples (s):** `[9.7053635, 9.9472901, 9.8607781, 9.8160658, 9.7872446]` (Median: 9.816s, Range: 9.705s–9.947s)
- **Public Repeat Samples (s):** `[0.6128365, 0.6494758, 0.6264859, 0.5698245, 0.5697436]` (Median: 0.613s, Range: 0.570s–0.649s)
- **Emitted Mirrored Native Scopes:** All 5 build pairs verified that all 30 leaf dependencies were transformed with native OpenTelemetry tracer scopes.

### Performance Budget Assessment & Recommendation
The optimizations reduced clean-build overhead from **117.3% down to 48.5%** (a 5.19-second / 34.6% wall-clock reduction). However, because Candidate A compiles unowned dependencies twice (once in the pre-pass to resolve authoritative OpenTelemetry `.rlib` metadata, and once in the instrumented pass), a $+40\%$ clean overhead budget is mathematically impossible for dependency instrumentation in non-trivial graphs.

**Recommendation:**
1. A defensible, realistic clean-build budget for Candidate A is **$\le 55\%$** (or $\le 60\%$).
2. In accordance with project instructions, the provisional limit of 40% was **not** artificially raised in code, and `cargo bench --bench bench_scale` exited with code 1. Acceptance of this item remains pending until the proposed 55% budget is formally approved.

---

## 3. Live Scale & Incremental Validation Suite

A new dedicated test suite was added at `cargo-instrument/tests/scale_incremental_e2e_tests.rs` covering large graphs, various topologies, and incremental workflows:

1. **Broad Graph (100 Unowned Dependencies Outside Workspace):**
   - 100 leaf crates excluded from workspace membership.
   - Built with `--with-dependencies` and executed with `InMemorySpanExporter`.
   - Verified that all 100 dependencies executed, generated spans with their crate names as instrumentation scopes, transformed into isolated mirrors, and left source files intact.
2. **Layered Diamond Graph (100 Dependencies):**
   - 4 layers of 25 crates each ($25 \times 4 = 100$ unowned crates) with multiple converging dependency paths.
   - Built under parallel compilation (`-j 4`).
   - Verified mirror isolation, DAG build ordering, and span emission.
3. **Deep Linear Chain (20 Dependencies):**
   - Strict linear dependency chain ($A \to B \to C \to \dots \to \text{leaf}$).
   - Verified recursive discovery, deep dependency resolution, and native mirror instrumentation.
4. **Incremental Cycles at Scale (10 Dependencies):**
   - **Cycle 1 (Clean Build):** Initial compile, verified all 10 leaf mirrors created, input tree snapshots identical.
   - **Cycle 2 (Repeat Build / No-op):** Sub-second build, zero mirror modifications, exact snapshot match.
   - **Cycle 3 (Application-only Edit):** Modified binary root `main.rs`. Verified dependencies were neither re-cleaned nor re-instrumented.
   - **Cycle 4 (Dependency-only Edit):** Modified `inc_leaf_0/src/lib.rs`. Verified that only `inc_leaf_0` and the application were recompiled and re-instrumented into the mirror with the updated code.
   - **Cycle 5 (Clean Rebuild):** Ran `cargo clean` and rebuilt with `--with-dependencies`. Verified complete restoration of all 10 leaf mirrors.
5. **Tier-2 Fallback at Scale (10 Dependencies):**
   - Profile mismatch (`opt-level = 1` vs `opt-level = 0`) forcing Tier-2 fallback across 10 unowned dependencies.
   - Verified selection of Tier-2 fallback and emission of C-ABI trampolines (`__otel_span_enter` and `__otel_span_exit`).
6. **First-Party Lint-Apply Scale Measurement:**
   - 5 workspace crates with 10 functions each (both sync and async, 100 functions total).
   - Applied AST rewrite using `cargo instrument-rust --apply` with pinned driver `nightly-2026-09-09`.
   - Verified insertion of `/* __cargo_instrument_rust:p23 */` markers and clean-worktree safety invariant.
   - Verified idempotency (second apply run produced zero diffs).
   - Timing: Completed 5 crates in **9.339s** (~1.87s per crate).

---

## 4. Tracer Acquisition Profiling

Evaluated via `tests/tracer_caching_profile_tests.rs` under release mode:
- **No-op Provider Acquisition:** 9.62–14.92 ns.
- **SDK Provider Acquisition:** 84.67–98.95 ns.
- **Cached Handle Access:** 0.26–0.35 ns.
- **Decision:** Static caching remains rejected. If a tracer handle is cached before an application initializes or replaces its tracer provider (common in telemetry initialization), the cached handle continues pointing to the no-op provider, silently dropping all subsequent spans. OpenTelemetry 0.32's `RwLock` lookup cost is acceptable given this correctness hazard. All 3 tracer tests (including multithreaded concurrency and dynamic provider invalidation) passed.

---

## 5. Remote Platform & Toolchain Evidence

Remote CI execution on GitHub Actions was inspected for the `main` branch at commit `b771da5`:

| Workflow | Run ID | Status | Platforms / Jobs Verified |
| --- | --- | --- | --- |
| **CI** | [36386894186](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/runs/36386894186) | Succeeded | `ubuntu-latest` (Linux, 4m21s), `macos-latest` (macOS, 7m8s), `windows-latest` (Windows, 10m12s). Ran `cargo fmt`, `cargo clippy -D warnings`, `cargo test --workspace`, `cargo build --workspace`. |
| **Integration** | [36386894301](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/runs/36386894301) | Succeeded | `ubuntu-latest` (2m43s), `macos-latest` (2m44s), `windows-latest` (2m53s). Ran real subprocess Cargo/rustc integration tests (`cargo_integration_tests`, `wrapper_tests`, `trampoline_tests`, `graph_topology_tests`). |
| **P2.3 HIR Apply** | [36386894178](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/runs/36386894178) | Succeeded | `ubuntu-latest` (1m10s). Installed pinned toolchain `nightly-2026-09-09` with `rustc-dev`, ran compiler driver tests and subcommand apply round trip. |

---

## 6. Local Workspace Validation Summary

| Command | Scope | Result |
| --- | --- | --- |
| `cargo fmt --all -- --check` | Entire workspace | PASSED (0 formatting diffs) |
| `cargo clippy --workspace --all-targets -- -D warnings` | All targets, all crates | PASSED (0 warnings) |
| `cargo test --workspace -- --test-threads=2` | Full workspace test suite | PASSED (all tests passed) |
| `cargo test -p cargo-instrument --test scale_incremental_e2e_tests` | 100-dep broad, diamond, chain, incremental, fallback | PASSED (5 passed, 1 ignored) |
| `cargo test -p cargo-instrument --test scale_incremental_e2e_tests test_first_party_lint_apply_scale_measurement -- --ignored` | Pinned nightly driver first-party apply scale | PASSED (1 passed, 9.34s) |
| `cargo test -p cargo-instrument --release --offline --test tracer_caching_profile_tests -- --nocapture` | Tracer acquisition & provider replacement | PASSED (3 passed) |
| `cargo bench -p cargo-instrument --bench bench_scale` | Scale & performance budget benchmark | Planning PASS, Repeat PASS, Clean Overhead 48.5% FAIL (vs 40% provisional limit) |

---

## 7. Acceptance Status & Closeout Conclusion

- **Graph Scale & Topologies:** Complete and validated up to 100 unowned dependencies in broad, deep, and layered diamond graphs.
- **Incremental Workflow:** Complete and validated across all 5 cycle stages.
- **First-Party Apply Scale:** Complete and validated using pinned toolchain (1.87s/crate).
- **Tracer Acquisition & Safety:** Complete; caching rejected for correctness reasons.
- **Platform Evidence:** Complete; verified green remote runs on Linux, Windows, macOS, and pinned nightly driver.
- **Clean-Build Overhead Budget:** Overhead significantly reduced from 117.3% to 48.5%. A budget of $\le 55\%$ is proposed based on Candidate A two-phase compilation realities. Because the provisional 40% limit was intentionally preserved, P2.5 remains open until the 55% budget is formally approved.