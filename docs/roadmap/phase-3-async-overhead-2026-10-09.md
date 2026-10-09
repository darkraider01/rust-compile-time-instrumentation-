# Phase 3 Overhead Evaluation — Async Workload & Task Spawning Measurement Note (2026-10-09)

[Project README](../../README.md) · [Phase 3 roadmap](phase-3-evaluation.md) · [Steady-state sync overhead](phase-3-overhead-pilot-2026-10-08-steady-state.md) · [Repeat-build reuse investigation](phase-3-repeat-build-reuse-investigation-2026-10-09.md)

Status: fourth Phase 3 overhead evaluation executed 2026-10-09 across three separate process sessions on one host. This evaluation extends the Phase 3 empirical record to an asynchronous workload: measuring the observable overhead of generated native OpenTelemetry dependency instrumentation for bounded concurrent tasks that perform deterministic useful work and suspend via cooperative executor yields.

Phase 3 remains open: this note covers a bounded, deterministic async fixture and trace oracle. It does not evaluate external collector backends, network batch exporters, or real-world application services.

---

## 1. Workload Design and Invariants (Variant A)

Following direction on the candidate workload proposals, the evaluation implemented the approved **Variant A** configuration exercising concurrent tasks spawned with `tokio::spawn`, async function boundaries, and cooperative executor yields:

1. **Workload inputs & deterministic useful work**:
   - Two async dependency functions in `async_dep`:
     - `step_a(x: u64) -> u64`: performs 64-bit wrapping multiplication and additions, yields cooperatively via `tokio::task::yield_now().await`, and returns transformed state.
     - `step_b(x: u64) -> u64`: performs 64-bit bitwise rotation, XOR mixing, yields cooperatively via `tokio::task::yield_now().await`, and returns transformed state with wrapping multiplication.
   - Initial accumulator seed per task $k \in 0..10$: `0x1234_5678_9ABC_DEF0 ^ (k as u64)`.
   - Iterations per task: $M = 1,000$.
   - At each iteration $i$, task $k$ evaluates:
     $$\text{acc} \leftarrow \text{step\_b}(\text{step\_a}(\text{acc} \oplus i))$$
   - Total pipeline invocations: $10 \times 1,000 = 10,000$.
   - Total dependency function calls: $10,000 \times 2 = 20,000$.
   - Total combined accumulator across all 10 tasks: exactly `EXPECTED_ACC = 9839328791058930113`.
   - Verified in both warm-up and measured passes to ensure compiler optimization does not eliminate execution.

2. **Concurrency & runtime model**:
   - Runtime: Tokio multi-threaded runtime (`#[tokio::main(flavor = "multi_thread", worker_threads = 2)]`).
   - Concurrency: $C = 10$ concurrent tasks spawned via `tokio::spawn`.
   - Worker threads fixed at 2 to avoid unconstrained CPU core scaling variance on the 12-vCPU host.
   - Suspension points: `tokio::task::yield_now().await` forces actual cooperative task suspension, context detach/reattach, and executor rescheduling at every step.

3. **In-process untimed warm-up protocol**:
   - Tracer provider and `CountingProcessor(Arc<AtomicU64>)` initialized once outside timing.
   - Untimed warm-up pass executes the full $C=10, M=1,000$ workload (20,000 calls).
   - Warm-up pass asserts span count: exactly `0` for baseline, exactly `20,000` for instrumented.
   - Warm-up pass asserts exact output agreement against `EXPECTED_ACC`.
   - Distinguishable verification line emitted: `WARMUP_VERIFIED spans=... expected=... acc=...`.
   - Counter reset: `span_count.store(0, Ordering::Relaxed)`.
   - Timed pass executed with `Instant::now()` and `elapsed()`.
   - Measured pass separately asserts span count and output agreement against `EXPECTED_ACC` and warm-up result (`MEASURED_VERIFIED`).

4. **Public CLI workflow and wrapper enforcement**:
   - Instrumented release build invoked explicitly via `cargo-instrument --with-dependencies -- build --release`.
   - Diagnostic output confirms that `async_dep` selected the native R-4 emitter (`crate=async_dep] selecting native R-4 emitter`).
   - Diagnostic output confirms that workspace application `async_app` was excluded from instrumentation.
   - Ambient instrumentation settings scrubbed from all child commands.

---

## 2. Trace Oracle and Correctness Verification

Prior to timed performance measurements, each compiled release binary was independently audited using `--verify-traces` with an `InMemorySpanExporter`:

1. **Independent oracle check**:
   - Application creates active root span `async_workload_root` with trace ID $T_{\text{root}}$ and span ID $S_{\text{root}}$.
   - Application invokes `step_a`, `step_b`, and `spawn_step` with parent context attached.
   - Inside `async_dep`, `spawn_step` calls `tokio::spawn(async move { step_b(x).await })`.
   - `cargo-instrument`'s native AST transformation rewrites `tokio::spawn` into `tokio::spawn(FutureExt::with_context(..., Context::current()))`.

2. **Trace assertions**:
   - **Baseline binary**: asserts that exactly 0 dependency spans are produced (`ORACLE_VERIFIED baseline=true spans=0`).
   - **Instrumented binary**: asserts that exactly 4 dependency spans are produced:
     1. `step_a`: `trace_id == T_root`, `parent_span_id == S_root`, `instrumentation_scope == "async_dep"`.
     2. `step_b`: `trace_id == T_root`, `parent_span_id == S_root`, `instrumentation_scope == "async_dep"`.
     3. `spawn_step`: `trace_id == T_root`, `parent_span_id == S_root`, `instrumentation_scope == "async_dep"`.
     4. `step_b` (spawned child): `trace_id == T_root`, `parent_span_id == span_spawn.span_id`, demonstrating context preservation across `tokio::spawn` within the dependency.
     5. Lifecycle outcome: each span carries attribute `cargo.instrumentation.async.outcome = "completed"`.
     6. Suspension context restoration: context remains attached and valid after resuming from `tokio::task::yield_now().await`.
   - Result: 4/4 spans verified across all sessions (`ORACLE_VERIFIED instrumented=true spans=4`).

---

## 3. Environment and Session Conditions

| Item | Value |
|---|---|
| Revision | `3102eb3bfe649fa9d1ad9e9b068ef0b3d11b3bc5`, branch `main`; dirty only by benchmark harness additions |
| Host / OS | Linux fedora 6.19.10-300.fc44.x86_64, `x86_64-unknown-linux-gnu` |
| Hardware | AMD Ryzen 5 5600H with Radeon Graphics (12 vCPUs), 18 GiB RAM, 39 GiB swap |
| Toolchain | `rustc 1.99.0 (b940084d7 2026-09-28)`, `cargo 1.99.0 (5f94df478 2026-08-27)` (stable) |
| Lockfile | Workspace `Cargo.lock` SHA-256 `c9c577e982fbc05b969d03853458df0300ca290130bf38364532a59145acb31c` |
| Network | `CARGO_NET_OFFLINE=true` for all sessions (offline local resolution) |
| Execution Command | `CARGO_NET_OFFLINE=true cargo bench -p cargo-instrument --bench bench_async_overhead` |
| Raw Evidence | `evidence/phase3-async-overhead-2026-10-09/` (see section 7) |

### Session execution conditions & failure disclosure

Three separate process sessions were executed sequentially on the host:

- **Session 1**:
  - Time window: 2026-10-08T23:06:38Z → 23:07:00Z (22 s), `BENCH_EXIT=0`
  - Load average: start `5.64 4.09 3.63`, end `5.40 4.18 3.67`
- **Session 2 (Initial Attempt — Excluded Failure)**:
  - Time window: 2026-10-08T23:07:04Z → 23:07:17Z (13 s), `BENCH_EXIT=101`
  - Failure cause: `rustc-LLVM ERROR: IO failure on output stream: Disk quota exceeded (os error 122)`.
  - Diagnosis: `tempfile::tempdir()` created temporary directories under system `/tmp` (tmpfs), where the user `brandybuck` had a strict quota limit of 7,911,337 blocks (~3.8 GB) with existing background artifacts occupying 3.4 GB. Compiling release artifacts pushed storage over quota.
  - Resolution: The benchmark harness was updated to create its isolated temporary directory under `target/` within the repository workspace on NVMe storage (334 GB available, no tmpfs quota). The failed run log was preserved as `evidence/phase3-async-overhead-2026-10-09/session2-failed-quota.log`.
- **Session 2 (Rerun)**:
  - Time window: 2026-10-08T23:15:52Z → 23:16:12Z (20 s), `BENCH_EXIT=0`
  - Load average: start `1.62 1.48 2.45`, end `1.68 1.53 2.47`
- **Session 3**:
  - Time window: 2026-10-08T23:17:51Z → 23:18:10Z (19 s), `BENCH_EXIT=0`
  - Load average: start `1.81 1.49 2.33`, end `2.12 1.55 2.35`

---

## 4. Detailed Measurement Results

All runtime measurements reflect alternating baseline-first and instrumented-first order across $N=10$ runs per session. Each run measured $C=10, M=1,000$ (10,000 pipeline units, 20,000 dependency calls).

### 4.1 Session 1 Raw Samples
| Run | Order | Baseline Total (ms) | Baseline Call (ns) | Baseline Pipe (ns) | Inst Total (ms) | Inst Call (ns) | Inst Pipe (ns) | Delta / Call (ns) |
|---|---|---|---|---|---|---|---|---|
| 0 | baseline_first | 1.99 | 99.4 | 198.8 | 6.88 | 344.2 | 688.3 | +244.8 |
| 1 | instrumented_first | 1.83 | 91.5 | 183.0 | 7.02 | 351.1 | 702.2 | +259.6 |
| 2 | baseline_first | 1.87 | 93.3 | 186.7 | 6.78 | 338.9 | 677.7 | +245.5 |
| 3 | instrumented_first | 1.83 | 91.5 | 183.0 | 7.01 | 350.5 | 701.1 | +259.0 |
| 4 | baseline_first | 1.87 | 93.3 | 186.5 | 6.81 | 340.6 | 681.3 | +247.3 |
| 5 | instrumented_first | 1.80 | 89.8 | 179.6 | 6.89 | 344.7 | 689.4 | +254.9 |
| 6 | baseline_first | 1.76 | 87.8 | 175.7 | 6.94 | 347.0 | 694.0 | +259.2 |
| 7 | instrumented_first | 1.82 | 91.2 | 182.4 | 7.39 | 369.3 | 738.6 | +278.1 |
| 8 | baseline_first | 1.91 | 95.4 | 190.8 | 6.87 | 343.7 | 687.4 | +248.3 |
| 9 | instrumented_first | 1.81 | 90.7 | 181.3 | 6.90 | 345.0 | 689.9 | +254.3 |

### 4.2 Session 2 Raw Samples
| Run | Order | Baseline Total (ms) | Baseline Call (ns) | Baseline Pipe (ns) | Inst Total (ms) | Inst Call (ns) | Inst Pipe (ns) | Delta / Call (ns) |
|---|---|---|---|---|---|---|---|---|
| 0 | baseline_first | 2.03 | 101.5 | 203.0 | 6.55 | 327.3 | 654.5 | +225.8 |
| 1 | instrumented_first | 1.85 | 92.4 | 184.8 | 6.53 | 326.6 | 653.2 | +234.2 |
| 2 | baseline_first | 1.85 | 92.6 | 185.2 | 6.35 | 317.7 | 635.3 | +225.1 |
| 3 | instrumented_first | 1.81 | 90.7 | 181.5 | 6.58 | 328.9 | 657.8 | +238.1 |
| 4 | baseline_first | 1.98 | 99.2 | 198.4 | 6.58 | 328.8 | 657.7 | +229.6 |
| 5 | instrumented_first | 1.87 | 93.3 | 186.6 | 6.47 | 323.5 | 647.0 | +230.2 |
| 6 | baseline_first | 1.68 | 84.0 | 168.1 | 6.64 | 332.1 | 664.1 | +248.1 |
| 7 | instrumented_first | 1.74 | 87.0 | 174.0 | 6.91 | 345.4 | 690.8 | +258.4 |
| 8 | baseline_first | 1.84 | 92.2 | 184.4 | 6.74 | 336.9 | 673.8 | +244.7 |
| 9 | instrumented_first | 1.90 | 95.2 | 190.4 | 6.59 | 329.5 | 659.0 | +234.3 |

### 4.3 Session 3 Raw Samples
| Run | Order | Baseline Total (ms) | Baseline Call (ns) | Baseline Pipe (ns) | Inst Total (ms) | Inst Call (ns) | Inst Pipe (ns) | Delta / Call (ns) |
|---|---|---|---|---|---|---|---|---|
| 0 | baseline_first | 1.70 | 85.0 | 170.0 | 6.43 | 321.4 | 642.7 | +236.4 |
| 1 | instrumented_first | 1.77 | 88.6 | 177.1 | 6.64 | 331.8 | 663.5 | +243.2 |
| 2 | baseline_first | 1.74 | 87.2 | 174.4 | 6.44 | 322.0 | 644.1 | +234.8 |
| 3 | instrumented_first | 1.74 | 87.0 | 174.0 | 6.51 | 325.6 | 651.1 | +238.6 |
| 4 | baseline_first | 1.70 | 84.8 | 169.5 | 6.53 | 326.6 | 653.2 | +241.8 |
| 5 | instrumented_first | 1.83 | 91.5 | 183.0 | 6.40 | 319.9 | 639.8 | +228.4 |
| 6 | baseline_first | 1.83 | 91.5 | 183.0 | 6.43 | 321.7 | 643.3 | +230.2 |
| 7 | instrumented_first | 1.95 | 97.3 | 194.7 | 6.30 | 315.2 | 630.4 | +217.9 |
| 8 | baseline_first | 1.79 | 89.5 | 179.1 | 6.20 | 310.2 | 620.5 | +220.7 |
| 9 | instrumented_first | 1.76 | 87.9 | 175.8 | 6.32 | 316.2 | 632.4 | +228.3 |

---

## 5. Statistical Summary and Cross-Session Comparison

| Metric | Session 1 | Session 2 | Session 3 | Pooled Summary Across Sessions |
|---|---|---|---|---|
| **Baseline median total (ms)** | 1.865 (min: 1.760, max: 1.990, spread: 0.230) | 1.850 (min: 1.680, max: 2.030, spread: 0.350) | 1.775 (min: 1.700, max: 1.950, spread: 0.250) | **1.775 – 1.865 ms** (spread: 0.230 – 0.350) |
| **Instrumented median total (ms)** | 6.900 (min: 6.780, max: 7.390, spread: 0.610) | 6.565 (min: 6.350, max: 6.910, spread: 0.560) | 6.435 (min: 6.200, max: 6.640, spread: 0.440) | **6.435 – 6.900 ms** (spread: 0.440 – 0.610) |
| **Duration delta (ms)** | +5.04 ms (+270.0%) | +4.71 ms (+254.9%) | +4.66 ms (+262.5%) | **+4.66 – +5.04 ms** (+255% to +270%) |
| **Baseline time / pipeline (ns)** | 186.6 (min: 175.7, max: 198.8, spread: 23.2) | 185.0 (min: 168.1, max: 203.0, spread: 34.9) | 177.5 (min: 169.5, max: 194.7, spread: 25.2) | **177.5 – 186.6 ns** (spread: 23.2 – 34.9) |
| **Instrumented time / pipeline (ns)** | 689.9 (min: 677.7, max: 738.6, spread: 60.9) | 656.7 (min: 635.3, max: 690.8, spread: 55.5) | 643.4 (min: 620.5, max: 663.5, spread: 43.0) | **643.4 – 689.9 ns** (spread: 43.0 – 60.9) |
| **Delta / pipeline (ns)** | +503.3 ns (+269.7%) | +471.7 ns (+255.0%) | +465.9 ns (+262.5%) | **+465.9 – +503.3 ns** (+255% to +270%) |
| **Baseline time / call (ns)** | 93.3 (min: 87.8, max: 99.4, spread: 11.6) | 92.5 (min: 84.0, max: 101.5, spread: 17.4) | 88.7 (min: 84.8, max: 97.3, spread: 12.6) | **88.7 – 93.3 ns** (spread: 11.6 – 17.4) |
| **Instrumented time / call (ns)** | 345.0 (min: 338.9, max: 369.3, spread: 30.4) | 328.4 (min: 317.7, max: 345.4, spread: 27.8) | 321.7 (min: 310.3, max: 331.8, spread: 21.5) | **321.7 – 345.0 ns** (spread: 21.5 – 30.4) |
| **Delta / dependency call (ns)** | **+251.7 ns** (+269.8%) | **+235.9 ns** (+255.0%) | **+233.0 ns** (+262.5%) | **+233.0 – +251.7 ns/call** (+255% to +270%) |
| **Release binary growth** | +45,800 bytes (+3.30%) | +45,864 bytes (+3.30%) | +45,872 bytes (+3.30%) | **+45,800 – +45,872 bytes** (~3.30%) |
| **Trace Oracle correctness** | 4/4 spans verified | 4/4 spans verified | 4/4 spans verified | **100% pass across all sessions** |

---

## 6. Interpretation, Tradeoffs, and Boundaries

### 6.1 Relation to Historical Synchronous Results

The synchronous in-process steady-state evaluation reported:
- Baseline duration: ~0.84 ms (500k calls $\rightarrow$ ~8.4 ns/call)
- Instrumented duration: ~25.2 ms (500k calls $\rightarrow$ ~252.4 ns/call)
- Steady-state delta: **+243.52 – +245.87 ns/call**
- Release binary growth: **+55,560 bytes (+7.1%)**

The async steady-state evaluation reports:
- Baseline duration: ~1.78 – 1.86 ms (20k calls $\rightarrow$ ~88.7 – 93.3 ns/call)
- Instrumented duration: ~6.43 – 6.90 ms (20k calls $\rightarrow$ ~321.7 – 345.0 ns/call)
- Steady-state delta: **+233.0 – +251.7 ns/call** (+465.9 – +503.3 ns per 2-step pipeline)
- Release binary growth: **+45,800 – +45,872 bytes (+3.30%)**

Key observations:
1. **Absolute runtime cost consistency**: Despite the fundamental difference in execution model (synchronous inline calls vs. asynchronous futures with cooperative yields on a 2-worker multi-threaded Tokio runtime), the absolute incremental runtime cost per dependency span remains strikingly consistent: **~233 to 252 ns per completed dependency invocation**.
2. **Percentage overhead divergence**: In the synchronous benchmark, baseline calls performed pure integer arithmetic (~8.4 ns/call), making the +244 ns instrumentation cost appear as a ~3,000% increase. In the async workload, each step performs genuine executor scheduling and cooperative suspension (`yield_now().await`), raising baseline execution time to ~90 ns/call. As a consequence, the percentage increase drops from ~3,000% to **~255%–270%**.
3. **Denominator clarity**:
   - Time per completed pipeline: measures the end-to-end execution of a logical transaction (`step_a` + `step_b`), showing an incremental cost of **+466 – +503 ns/pipeline**.
   - Time per dependency call: measures an individual async function boundary with one cooperative yield, showing **+233 – +252 ns/call**.

### 6.2 Why Nanoseconds per Span Must Not Be Oversimplified

Nanoseconds per span cannot be interpreted as a flat tax that scales linearly in all async contexts:
1. **Poll frequency vs. span creation**: In the native R-4 async transformation, the span is created once upon future construction, attached on each `poll()`, detached on `Pending` / yield, and ended when `Drop` runs on completion. If a future yields 100 times before completing, the context attach/detach runs 100 times while span construction and termination run once. In this fixture, each step suspends exactly once, so the measured delta reflects 1 construction, 2 polls, 1 detach, and 1 completion.
2. **Concurrency and counting**: Timing occurs across 10 concurrent tasks on 2 worker threads. Thread synchronization inside `CountingProcessor`'s `AtomicU64` occurs within the timed block.
3. **Absence of exporter**: Measurements do not include batch exporting, network serialization, or collector queueing; they measure in-process OpenTelemetry SDK span processor recording overhead.

---

## 7. Durable Evidence Manifest

All raw benchmark execution outputs, environment state, and harness patches are retained in `evidence/phase3-async-overhead-2026-10-09/`:

| File | SHA-256 Hash | Description |
|---|---|---|
| `env-manifest.txt` | `a6b69919906ee1a194d1fdd6d6ec4958c51397b414cd4a624941ee4b77ffc5d3` | Host metadata, toolchain identity, CPU flags, and lockfile SHA-256 |
| `harness.patch` | `c44d748ee15f74536a4b1eeaf7ea3c42c164354fb9cd2bb420a5d6e1b6ebe394` | Exact diff adding `bench_async_overhead` to `cargo-instrument` |
| `run-bench.sh` | `8718d6c1b1de14db0a9d6d02533d3a46ea7a23387d79a3f91bca55fd8bde02cb` | Shell runner script executing sessions with UTC timestamps and load averages |
| `session1.log` | `b3f4985672412124e1790f90a1b74aacfe406f694615cd41c588768aa9b47d43` | Session 1 complete execution log (exit status 0) |
| `session2-failed-quota.log` | `9193c9b443244d97f09abfdcd351d980e2c74f340b3db3f9f17e52517b7354a1` | Session 2 initial attempt log (exit status 101, preserved failure disclosure) |
| `session2.log` | `2f403a5d1fca3357daa381450e877532f5b0938f07cbd683a643f80822ba2d2b` | Session 2 successful rerun execution log (exit status 0) |
| `session3.log` | `687c7bbc3374c11ad140667254a721e225d25fb4bb48d40ccfc74e78abc2c76e` | Session 3 complete execution log (exit status 0) |
