# Phase 3 Overhead Evaluation — In-Process Steady-State Measurement Note (2026-10-08)

[Project README](../../README.md) · [Phase 3 roadmap](phase-3-evaluation.md) · [Historical pilot (2026-10-07)](phase-3-overhead-pilot-2026-10-07.md) · [Fresh-process evaluation (2026-10-08)](phase-3-overhead-pilot-2026-10-08.md)

Status: third Phase 3 overhead evaluation executed 2026-10-08 across three separate process sessions on one host. This note advances the methodology by implementing genuine **in-process untimed workload warm-up** inside each measured application child process, verifying warm-up span counts and exact workload output agreement, and resetting counters and accumulator state prior to starting the timer. The old separate-process external warm-up has been removed.

Phase 3 remains open: this note covers a bounded, deterministic synchronous fixture only. It does not measure async workloads, exporter pipelines, real-application graphs, or first-party HIR transformations.

---

## 1. Protocol and Harness Refinements

Following the warm-up findings on the earlier fresh-process protocol, the benchmark harness (`cargo-instrument/benches/bench_overhead.rs`) was updated with the authorized minimal changes:

1. **In-process untimed workload warm-up**:
   - Each measured application child process executes an untimed workload pass of identical volume ($M=100,000$ iterations $\times 5$ dependency calls = 500,000 calls) before timing begins.
   - Telemetry provider, OpenTelemetry SDK tracer provider, and counting processor are initialized once outside both passes.
2. **Warm-up verification**:
   - The warm-up pass asserts that dependency span counts match expectations: exactly `0` completed spans for baseline, exactly `500,000` completed spans for instrumented.
   - The warm-up pass asserts exact observable workload output agreement (`acc == 2033737868462570593`).
   - Clearly distinguishable verification line printed: `WARMUP_VERIFIED spans=... expected=... acc=...`.
3. **Counter and state reset**:
   - Prior to timing, the atomic span counter is reset to zero (`span_count.store(0, Ordering::Relaxed)`).
   - Workload input state is reset so the timed pass begins from the exact same initial accumulator value (`acc = 1`).
4. **Timed measured pass**:
   - The timer (`Instant::now()`) starts only after warm-up verification and counter reset.
   - The measured workload runs for the same $M=100,000$ iterations.
   - Timing stops immediately upon completion of the workload before any assertions or output.
5. **Measured verification & separation of counts**:
   - The measured pass separately verifies span count (`500,000` for instrumented, `0` for baseline).
   - Workload output is verified against expected output and checked for agreement with the warm-up pass.
   - Printed verification line: `MEASURED_VERIFIED spans=... expected=... acc=...`.
   - Warm-up and measured span counts are strictly isolated; a failure to reset counters would yield 1,000,000 spans and fail the assertion.
6. **Removal of external separate-process warm-up**:
   - The old separate child-process warm-up and its discarded timing output were removed from `bench_overhead.rs`.
   - The harness verifies that every measured child process emitted both `WARMUP_VERIFIED` and `MEASURED_VERIFIED` tags before parsing `RUNTIME_RESULT`.
7. **Sample counts & alternating execution order preserved**:
   - Build scenarios (clean, repeat, incremental app edit): $N=5$ measured pairs per scenario.
   - Runtime scenario: $N=10$ measured pairs per variant.
   - Even runs ($run \% 2 == 0$): baseline executed first, instrumented second.
   - Odd runs ($run \% 2 == 1$): instrumented executed first, baseline second.
8. **Clean-build and environment integrity preserved**:
   - Isolated target directories per clean run (`target_base_{run}` and `target_inst_{run}`).
   - Explicit `--with-dependencies` public CLI workflow (`dependencies-v1`).
   - Ambient variable scrubbing (`CARGO_INSTRUMENT_*`, `INSTRUMENT_DEBUG`, `RUSTC_WRAPPER`, etc.).
   - Release build native R-4 emitter verification on `bench_dep` and exclusion verification on `bench_app`.

---

## 2. Environment and Sessions

| Item | Value |
| --- | --- |
| Revision | `2bcc329ed195a4f70d751c19fa623b626829853e`, branch `main`; dirty only by harness changes (`evidence/phase3-overhead-2026-10-08-steady-state/harness.patch`) |
| Host / OS | Linux fedora 6.19.10-300.fc44.x86_64, `x86_64-unknown-linux-gnu` |
| Hardware | AMD Ryzen 5 5600H with Radeon Graphics (12 vCPUs), 18 GiB RAM, 39 GiB swap |
| Toolchain | `rustc 1.99.0 (b940084d7 2026-09-28)`, `cargo 1.99.0 (5f94df478 2026-08-27)` (stable) |
| Lockfile | Workspace `Cargo.lock` SHA-256 `c9c577e982fbc05b969d03853458df0300ca290130bf38364532a59145acb31c` |
| Network | `CARGO_NET_OFFLINE=true` for all sessions (offline local resolution) |
| Execution Command | `CARGO_NET_OFFLINE=true cargo bench -p cargo-instrument --bench bench_overhead` |
| Raw Evidence | `evidence/phase3-overhead-2026-10-08-steady-state/` (see section 7) |

### Session execution conditions

Three separate process sessions were executed sequentially on the same host without synthetic cooling periods, thread pinning, or background application termination:

- **Session 1**:
  - Time window: 2026-10-08T15:26:12Z → 15:28:01Z (109 s), `BENCH_EXIT=0`
  - Load average: start `6.96 3.01 1.77`, end `5.52 3.93 2.27`
- **Session 2**:
  - Time window: 2026-10-08T15:28:20Z → 15:30:01Z (101 s), `BENCH_EXIT=0`
  - Load average: start `4.10 3.71 2.23`, end `4.01 3.85 2.43`
- **Session 3**:
  - Time window: 2026-10-08T15:30:14Z → 15:31:55Z (101 s), `BENCH_EXIT=0`
  - Load average: start `3.12 3.66 2.39`, end `3.63 3.83 2.60`

---

## 3. Detailed Results

### 3.1 Individual build samples (s)

#### Session 1
| # | Execution Order | Clean Base (s) | Clean Inst (s) | Repeat Base (s) | Repeat Inst (s) | Incr Base (s) | Incr Inst (s) |
|---|---|---|---|---|---|---|---|
| 0 | baseline_first | 7.260 | 6.818 | 0.031 | 0.181 | 0.140 | 0.295 |
| 1 | instrumented_first | 8.585 | 7.257 | 0.031 | 0.183 | 0.139 | 0.291 |
| 2 | baseline_first | 7.755 | 7.658 | 0.031 | 0.183 | 0.144 | 0.294 |
| 3 | instrumented_first | 6.538 | 6.966 | 0.030 | 0.183 | 0.141 | 0.331 |
| 4 | baseline_first | 6.391 | 6.814 | 0.031 | 0.183 | 0.143 | 0.295 |

#### Session 2
| # | Execution Order | Clean Base (s) | Clean Inst (s) | Repeat Base (s) | Repeat Inst (s) | Incr Base (s) | Incr Inst (s) |
|---|---|---|---|---|---|---|---|
| 0 | baseline_first | 6.967 | 6.533 | 0.033 | 0.179 | 0.142 | 0.289 |
| 1 | instrumented_first | 6.161 | 6.556 | 0.030 | 0.180 | 0.138 | 0.294 |
| 2 | baseline_first | 6.175 | 6.485 | 0.030 | 0.180 | 0.140 | 0.293 |
| 3 | instrumented_first | 6.580 | 6.513 | 0.030 | 0.181 | 0.139 | 0.295 |
| 4 | baseline_first | 6.328 | 6.785 | 0.032 | 0.181 | 0.140 | 0.296 |

#### Session 3
| # | Execution Order | Clean Base (s) | Clean Inst (s) | Repeat Base (s) | Repeat Inst (s) | Incr Base (s) | Incr Inst (s) |
|---|---|---|---|---|---|---|---|
| 0 | baseline_first | 6.759 | 6.560 | 0.031 | 0.183 | 0.143 | 0.291 |
| 1 | instrumented_first | 6.298 | 6.462 | 0.030 | 0.183 | 0.140 | 0.289 |
| 2 | baseline_first | 6.296 | 6.725 | 0.031 | 0.182 | 0.139 | 0.293 |
| 3 | instrumented_first | 6.338 | 6.497 | 0.031 | 0.185 | 0.139 | 0.291 |
| 4 | baseline_first | 6.196 | 6.622 | 0.031 | 0.182 | 0.141 | 0.294 |

---

### 3.2 Individual runtime samples (ns/call, M=100,000 iterations = 500,000 calls)

All individual measured samples were preceded by an in-process untimed warm-up pass verifying span counts and exact accumulator output (`acc == 2033737868462570593`).

| # | Execution Order | Session 1 Base | Session 1 Inst | Session 2 Base | Session 2 Inst | Session 3 Base | Session 3 Inst |
|---|---|---|---|---|---|---|---|
| 0 | baseline_first | 1.23 | 246.73 | 1.24 | 241.02 | 1.25 | 244.79 |
| 1 | instrumented_first | 1.26 | 249.41 | 1.24 | 243.65 | 1.22 | 244.72 |
| 2 | baseline_first | 1.25 | 249.53 | 1.22 | 243.82 | 1.39 | 245.11 |
| 3 | instrumented_first | 1.30 | 243.95 | 1.28 | 246.07 | 1.24 | 241.45 |
| 4 | baseline_first | 1.24 | 247.48 | 1.24 | 244.77 | 1.26 | 242.93 |
| 5 | instrumented_first | 1.24 | 249.49 | 1.24 | 251.26 | 1.23 | 248.31 |
| 6 | baseline_first | 1.23 | 246.63 | 1.25 | 245.76 | 1.22 | 243.44 |
| 7 | instrumented_first | 1.22 | 248.32 | 1.23 | 242.96 | 1.21 | 248.52 |
| 8 | baseline_first | 1.24 | 246.35 | 1.26 | 248.21 | 1.52 | 242.12 |
| 9 | instrumented_first | 1.23 | 244.78 | 1.22 | 251.95 | 1.22 | 254.27 |

---

### 3.3 Statistical summary per session

Spread is defined explicitly as:
$$\text{Spread} = \frac{\max - \min}{\text{median}}$$

#### Compile-time overhead summary ($N=5$ each, medians reported)

| Scenario | Session | Baseline Median (min, max, spread) | Instrumented Median (min, max, spread) | Absolute Delta | Overhead Delta (%) |
|---|---|---|---|---|---|
| **Clean Build** | 1 | 7.260 s (6.391, 8.585, 30.2%) | 6.966 s (6.814, 7.658, 12.1%) | -0.294 s | -4.0% |
| | 2 | 6.328 s (6.161, 6.967, 12.7%) | 6.533 s (6.485, 6.785, 4.6%) | +0.205 s | +3.2% |
| | 3 | 6.298 s (6.196, 6.759, 8.9%) | 6.560 s (6.462, 6.725, 4.0%) | +0.262 s | +4.2% |
| **Repeat Build** | 1 | 0.031 s (0.030, 0.031, 3.2%) | 0.183 s (0.181, 0.183, 1.1%) | +0.152 s | +496.4% |
| | 2 | 0.030 s (0.030, 0.033, 10.0%) | 0.180 s (0.179, 0.181, 1.1%) | +0.150 s | +491.0% |
| | 3 | 0.031 s (0.030, 0.031, 3.2%) | 0.183 s (0.182, 0.185, 1.6%) | +0.152 s | +488.2% |
| **Incremental (App)** | 1 | 0.141 s (0.139, 0.144, 3.5%) | 0.295 s (0.291, 0.331, 13.6%) | +0.154 s | +109.5% |
| | 2 | 0.140 s (0.138, 0.142, 2.9%) | 0.294 s (0.289, 0.296, 2.4%) | +0.154 s | +109.8% |
| | 3 | 0.140 s (0.139, 0.143, 2.9%) | 0.291 s (0.289, 0.294, 1.7%) | +0.151 s | +108.0% |

#### Runtime overhead summary ($N=10$ each after in-process warm-up and reset)

| Session | Baseline Median (min, max, spread) | Instrumented Median (min, max, spread) | Absolute Delta | Overhead Delta (%) |
|---|---|---|---|---|
| 1 | 1.240 ns (1.220, 1.300, 6.5%) | 247.105 ns (243.950, 249.530, 2.3%) | +245.865 ns/call | +19,827.8% |
| 2 | 1.240 ns (1.220, 1.280, 4.8%) | 245.265 ns (241.020, 251.950, 4.5%) | +244.025 ns/call | +19,679.4% |
| 3 | 1.235 ns (1.210, 1.520, 25.1%) | 244.755 ns (241.450, 254.270, 5.2%) | +243.520 ns/call | +19,718.2% |

#### Release binary size (Release profile)

Across all three sessions, the resulting release executables produced identical binary sizes:
- Baseline binary: 779,568 bytes
- Instrumented binary: 835,128 bytes
- Delta: +55,560 bytes (+7.1%)

*(Note: compared to the October 8 fresh-process evaluation binaries of 778,728 / 834,296 bytes, the baseline binary grew by +840 bytes and the instrumented binary grew by +832 bytes; this 8-byte difference explains why the instrumentation delta changed from +55,568 to +55,560 bytes. The binary growth reflects code additions in `bench_app/src/main.rs` (the local `run_workload` loop helper, constant accumulator checking, and in-process assertions), though their separate individual contributions were not independently measured).*

---

## 4. Cross-Session Variation and Comparative Analysis

### 4.1 Cross-session variability

- **Clean build compile medians**:
  - Baseline medians: 6.298 s, 6.328 s, 7.260 s $\rightarrow$ cross-session spread: **15.3%**
  - Instrumented medians: 6.533 s, 6.560 s, 6.966 s $\rightarrow$ cross-session spread: **6.6%**
  - Observations: Absolute clean build durations varied by up to 1.0 s across sessions. Observed deltas changed sign between sessions (-4.0% / -0.294 s in Session 1, +3.2% / +0.205 s in Session 2, +4.2% / +0.262 s in Session 3) and remained smaller than inter-session and within-session timing variations. Clean compilation overhead remains **inconclusive**.
- **Repeat and incremental compile deltas**:
  - Repeat build absolute delta across sessions: **+0.150 s, +0.152 s, +0.152 s** (150–152 ms).
  - Incremental app-edit absolute delta across sessions: **+0.151 s, +0.154 s, +0.154 s** (151–154 ms).
  - Observations: The absolute wall-time penalty of `cargo-instrument` CLI invocation (JSON pre-pass, candidate discovery, environment scrubbing, mirror preparation) is extremely steady across sessions at **~150–154 ms** for both repeat builds and incremental single-file application modifications.
- **Runtime per-call overhead**:
  - Baseline runtime medians: 1.235 ns, 1.240 ns, 1.240 ns $\rightarrow$ cross-session spread: **0.4%**
  - Instrumented runtime medians: 244.755 ns, 245.265 ns, 247.105 ns $\rightarrow$ cross-session spread: **1.0%**
  - Observations: Under the revised protocol, runtime per-call overhead deltas were observed tightly between **+243.52 and +245.87 ns/call** across the three session medians (cross-session spread: 1.0%; within-session spread: 2.3%–5.2%). While these values are lower than the earlier fresh-process results (+261.6 to +288.5 ns/call), this difference is reported as an observation under the revised protocol rather than establishing that warm-up alone caused the improvement, as host background conditions and binary layout changed as well.

### 4.2 Comparison across evaluation methodologies

*Caution*: In-process warm-up changes the runtime execution model and release binary code layout. Results are related, but not directly interchangeable.

| Metric | October 7 Pilot (historical, fresh process) | October 8 Evaluation (fresh-process sampling) | October 8 Evaluation (in-process steady-state) | Methodological differences |
|---|---|---|---|---|
| Runtime Warm-up Model | None | Separate external child process | Untimed in-process pass before counter reset | Primes in-process provider & processor structures |
| Runtime per-call delta | +249.34 ns/call | +261.60 / +269.84 / +288.53 ns/call | **+243.52 / +244.02 / +245.87 ns/call** | Lower delta observed under revised protocol; host conditions and binary layout also changed |
| Runtime sample count | N=5 | N=10 | N=10 | 10 pairs per session |
| Pair execution order | Fixed (baseline first) | Alternating | Alternating | Mitigates fixed-order host caching bias |
| Clean build overhead | +4.0% (+0.256 s) | -2.3% / +1.8% / +0.6% (inconclusive) | **-4.0% / +3.2% / +4.2% (inconclusive)** | Deltas change sign; smaller than host variation |
| Repeat build delta | +0.150 s (+150 ms) | +0.170 / +0.171 / +0.184 s | **+0.150 / +0.152 / +0.152 s** | Consistent ~150 ms invocation cost |
| Incremental build delta | +0.150 s (+150 ms) | +0.172 / +0.182 / +0.185 s | **+0.151 / +0.154 / +0.154 s** | Consistent ~151–154 ms invocation cost |
| Release binary growth | +55,560 bytes (+7.1%) | +55,568 bytes (+7.1%) | **+55,560 bytes (+7.1%)** | Release byte growth consistent across all three runs |

---

## 5. Correctness and Emitter Verification

All three sessions verified expected instrumentation contracts:

1. **Native R-4 Emitter Route**:
   Captured from release build stderr and asserted in each session:
   ```text
   EMITTER: [cargo-instrument PID=... crate=bench_dep] selecting native R-4 emitter
   EMITTER: [cargo-instrument PID=... crate=bench_dep] transformed 20 candidates into mirror ...
   ```
2. **Workspace Application Exclusion**:
   Asserted that `bench_app` produced zero wrapper transform lines under `--with-dependencies`.
3. **Exact Span Assertions (Both Passes)**:
   - Every instrumented runtime sample verified `WARMUP_VERIFIED spans=500000 expected=500000`.
   - Every instrumented runtime sample verified `MEASURED_VERIFIED spans=500000 expected=500000`.
   - Every baseline runtime sample verified `WARMUP_VERIFIED spans=0 expected=0`.
   - Every baseline runtime sample verified `MEASURED_VERIFIED spans=0 expected=0`.
4. **Exact Workload Output Agreement**:
   - Both passes for both variants produced identical accumulator output `acc == 2033737868462570593`.
5. **Execution Status**:
   All three benchmark sessions exited with `0`.

---

## 6. Limitations

- **Clean-build overhead remains inconclusive**: Across all three sessions, clean build deltas changed sign (-4.0% to +4.2%) and remained smaller than background host timing variation (up to 1.0 s). Clean compilation overhead cannot be isolated from background system variance on this fixture.
- **Warm-up scope**: While untimed in-process warm-up exercises the code path, initializes thread-local state, and populates processor caches, it does not guarantee that all OS scheduling, frequency scaling, or background CPU memory contention are stabilized.
- **Bounded synchronous fixture only**: Only exercises a synchronous call loop with an in-memory counting processor. Does not exercise asynchronous tasks, Tokio task migrations, exporter flushes, or network transport.
- **Single host**: All three sessions were conducted on one Linux host under realistic desktop load (loadavg 3.12–6.96). They reflect process session independence, not hardware or platform independence.
- **Host memory and swap allocation**: During the runs, the host reported 14 GiB of 18 GiB RAM in use (4.5 GiB available) and 13 GiB of 39 GiB swap allocated.
- **Timing composition**: Compile times reflect whole-command wall time (including cargo-instrument wrapper bootstrap, JSON metadata analysis, and cargo invocations). Internal substeps were not individually instrumented or subtracted.
- **No general performance claims**: These numbers represent pilot fixture measurements and do not establish production performance budgets or SLA guarantees.

---

## 7. Evidence Location and Verification

Raw evidence is preserved in [`evidence/phase3-overhead-2026-10-08-steady-state/`](../../evidence/phase3-overhead-2026-10-08-steady-state/):

| File | SHA-256 Checksum | Description |
|---|---|---|
| `env-manifest.txt` | `df9f1be3c439b036756ea0f79ec406922bbd1b8e17666e3b8a4a6d03a24f52c7` | Host environment, CPU model (`lscpu`), memory, toolchain, lockfile identity |
| `harness.patch` | `1d63bacf55a78da86b0f878cc1d72d650052468cda80a32814a70987cdfd2487` | Captured diff for `cargo-instrument/benches/bench_overhead.rs` implementing in-process warm-up |
| `run-bench.sh` | `3f3854082464179cb96c16ffb79897b2f1e105a20f95ba16f5835b86765abac9` | Exact execution script with timestamp and loadavg logging |
| `session1.log` | `77697d9a3999e48e1c173e40cd0ba6e599622346ebadd38f71700275e6afc5bd` | Session 1 complete raw stdout/stderr |
| `session2.log` | `ddf89f9203ba811b7dc0bea8dd90b9271f1a67ec6bb93f47c8d0607c71edc0e7` | Session 2 complete raw stdout/stderr |
| `session3.log` | `25fa12bf29b5666db58e2b2cc6f8e35452df181ab08609a9bfa3c9b14961f139` | Session 3 complete raw stdout/stderr |

### How to rerun

```sh
# Verification of benchmark compilation (offline):
CARGO_NET_OFFLINE=true cargo bench -p cargo-instrument --bench bench_overhead --no-run

# Workflow regression tests:
cargo test -p cargo-instrument --test dependency_instrumentation_e2e_tests

# Full benchmark execution (outputs to target/phase3-overhead-rerun/ by default to preserve committed session logs):
./evidence/phase3-overhead-2026-10-08-steady-state/run-bench.sh 1
```

---

## 8. Next Bounded Experiment

With the in-process warm-up methodology established and steady-state baselines collected across three sessions, the synchronous fixture measurements show a repeatable **~150–154 ms** compile-time penalty for `cargo-instrument` repeat and incremental builds, and a steady-state runtime penalty of **~244–246 ns/call** for synchronous SDK counting.

The next task will be **profiling the observed repeat-build overhead** to locate the demonstrated bottleneck (JSON pre-pass vs. CLI bootstrap vs. candidate discovery).
