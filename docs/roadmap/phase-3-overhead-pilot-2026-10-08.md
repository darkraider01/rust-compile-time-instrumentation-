# Phase 3 Overhead Evaluation — Three-Session Measurement Note (2026-10-08)

[Project README](../../README.md) · [Phase 3 roadmap](phase-3-evaluation.md) · [Historical pilot (2026-10-07)](phase-3-overhead-pilot-2026-10-07.md)

Status: second Phase 3 overhead evaluation executed 2026-10-08 across three separate process sessions on one host. This note strengthens the October 7 pilot by implementing alternating execution order across all pairs, doubling measured runtime samples to N=10, adding a discarded runtime warm-up, and collecting measurements across three distinct sessions without terminating desktop applications.

Phase 3 remains open: this note covers a bounded, deterministic synchronous fixture only. It does not measure async workloads, exporter pipelines, real-application graphs, or first-party HIR transformations.

---

## 1. Protocol and Harness Refinements

Following the gaps identified in the [historical pilot](phase-3-overhead-pilot-2026-10-07.md), the benchmark harness (`cargo-instrument/benches/bench_overhead.rs`) was updated with the authorized minimal changes:

1. **Sample counts**:
   - Build scenarios (clean, repeat, incremental app edit): N=5 measured samples per scenario.
   - Runtime scenario: N=10 measured samples per variant (M=100,000 iterations = 500,000 function calls per sample).
2. **Alternating pair execution order**:
   - Even runs (`run % 2 == 0`, runs 0, 2, 4, 6, 8): baseline executed first, instrumented second.
   - Odd runs (`run % 2 == 1`, runs 1, 3, 5, 7, 9): instrumented executed first, baseline second.
   - Each sample retains its explicit variant label and execution order (`order=baseline_first` or `order=instrumented_first`).
3. **Discarded runtime warm-up**:
   - Exactly one warm-up execution per variant before collecting measured runtime samples.
   - Warm-up executions run under full assertions (`EXPECTED_SPANS=0` for baseline, `EXPECTED_SPANS=500000` for instrumented) and print with explicit `WARMUP (discarded):` tags.
   - Warm-up timings never enter medians or sample vectors.
4. **Clean-build integrity preserved**:
   - No warm-up applied to clean builds; each clean sample compiles into a newly allocated, empty target directory (`target_base_{run}` and `target_inst_{run}`).
5. **Preserved repository conventions**:
   - Explicit `--with-dependencies` public CLI workflow (`dependencies-v1`).
   - Scrubbing of ambient `CARGO_INSTRUMENT_*`, `INSTRUMENT_DEBUG`, `RUSTC_WRAPPER`, and fault-injection variables.
   - Release build native R-4 route assertion on `bench_dep` and exclusion assertion on `bench_app`.
   - Release executable binary size measurement.

---

## 2. Environment and Sessions

| Item | Value |
| --- | --- |
| Revision | `577893c99b6f9387d6374078be4e5aeebbd0a1ca`, branch `main`; dirty only by uncommitted harness changes (`evidence/phase3-overhead-2026-10-08/harness.patch`) |
| Host / OS | Linux fedora 6.19.10-300.fc44.x86_64, `x86_64-unknown-linux-gnu` |
| Hardware | AMD Ryzen 5 5600H with Radeon Graphics (12 vCPUs), 18 GiB RAM, 39 GiB swap |
| Toolchain | `rustc 1.99.0 (b940084d7 2026-09-28)`, `cargo 1.99.0` (stable) |
| Lockfile | Workspace `Cargo.lock` SHA-256 `c9c577e982fbc05b969d03853458df0300ca290130bf38364532a59145acb31c` (offline resolution from registry cache) |
| Network | `CARGO_NET_OFFLINE=true` for all sessions |
| Execution Command | `CARGO_NET_OFFLINE=true cargo bench -p cargo-instrument --bench bench_overhead` |
| Raw Evidence | `evidence/phase3-overhead-2026-10-08/` (durable, inside repository; see section 7) |

### Session execution conditions

Three separate process sessions were executed sequentially on the same host without artificial cooldown periods, process killing, or host reconfiguration:

- **Session 1**:
  - Time window: 2026-10-08T14:21:44Z → 14:24:03Z (139 s), `BENCH_EXIT=0`
  - Load average: start `5.18 4.84 3.48`, end `7.74 5.86 4.04`
- **Session 2**:
  - Time window: 2026-10-08T14:24:31Z → 14:26:41Z (130 s), `BENCH_EXIT=0`
  - Load average: start `6.23 5.66 4.03`, end `8.45 7.38 4.92`
- **Session 3**:
  - Time window: 2026-10-08T14:26:49Z → 14:28:48Z (119 s), `BENCH_EXIT=0`
  - Load average: start `7.53 7.22 4.89`, end `5.16 6.56 4.94`

---

## 3. Detailed Results

### 3.1 Individual build samples (s)

#### Session 1
| # | Execution Order | Clean Base (s) | Clean Inst (s) | Repeat Base (s) | Repeat Inst (s) | Incr Base (s) | Incr Inst (s) |
|---|---|---|---|---|---|---|---|
| 0 | baseline_first | 9.268 | 9.009 | 0.037 | 0.222 | 0.171 | 0.355 |
| 1 | instrumented_first | 9.157 | 8.813 | 0.039 | 0.220 | 0.179 | 0.371 |
| 2 | baseline_first | 8.872 | 8.944 | 0.038 | 0.223 | 0.177 | 0.362 |
| 3 | instrumented_first | 9.162 | 8.998 | 0.040 | 0.222 | 0.179 | 0.377 |
| 4 | baseline_first | 8.790 | 8.871 | 0.036 | 0.217 | 0.177 | 0.356 |

#### Session 2
| # | Execution Order | Clean Base (s) | Clean Inst (s) | Repeat Base (s) | Repeat Inst (s) | Incr Base (s) | Incr Inst (s) |
|---|---|---|---|---|---|---|---|
| 0 | baseline_first | 9.090 | 8.984 | 0.034 | 0.206 | 0.156 | 0.339 |
| 1 | instrumented_first | 8.540 | 9.057 | 0.034 | 0.205 | 0.157 | 0.358 |
| 2 | baseline_first | 7.524 | 8.134 | 0.033 | 0.204 | 0.168 | 0.351 |
| 3 | instrumented_first | 8.824 | 9.420 | 0.035 | 0.207 | 0.157 | 0.325 |
| 4 | baseline_first | 9.722 | 8.806 | 0.036 | 0.204 | 0.158 | 0.326 |

#### Session 3
| # | Execution Order | Clean Base (s) | Clean Inst (s) | Repeat Base (s) | Repeat Inst (s) | Incr Base (s) | Incr Inst (s) |
|---|---|---|---|---|---|---|---|
| 0 | baseline_first | 8.022 | 7.771 | 0.037 | 0.208 | 0.156 | 0.329 |
| 1 | instrumented_first | 7.628 | 7.647 | 0.035 | 0.204 | 0.157 | 0.334 |
| 2 | baseline_first | 7.626 | 7.671 | 0.035 | 0.205 | 0.158 | 0.323 |
| 3 | instrumented_first | 7.579 | 7.531 | 0.034 | 0.208 | 0.163 | 0.339 |
| 4 | baseline_first | 7.570 | 7.774 | 0.035 | 0.202 | 0.157 | 0.327 |

---

### 3.2 Individual runtime samples (ns/call, M=100,000 iterations = 500,000 calls)

#### Warm-up (discarded)
- **Session 1**: Baseline 1.38 ns/call | Instrumented 286.16 ns/call
- **Session 2**: Baseline 1.32 ns/call | Instrumented 268.38 ns/call
- **Session 3**: Baseline 1.49 ns/call | Instrumented 263.67 ns/call

#### Measured samples

| # | Execution Order | Session 1 Base | Session 1 Inst | Session 2 Base | Session 2 Inst | Session 3 Base | Session 3 Inst |
|---|---|---|---|---|---|---|---|
| 0 | baseline_first | 1.37 | 285.56 | 1.35 | 265.27 | 1.51 | 252.09 |
| 1 | instrumented_first | 1.50 | 294.60 | 1.37 | 272.43 | 1.48 | 266.38 |
| 2 | baseline_first | 1.43 | 292.01 | 1.38 | 269.25 | 1.25 | 264.91 |
| 3 | instrumented_first | 1.40 | 279.89 | 1.30 | 264.83 | 1.27 | 261.02 |
| 4 | baseline_first | 1.40 | 287.85 | 1.28 | 270.26 | 1.52 | 255.87 |
| 5 | instrumented_first | 1.41 | 284.51 | 1.32 | 274.74 | 1.46 | 256.54 |
| 6 | baseline_first | 1.38 | 283.38 | 1.31 | 272.09 | 1.53 | 267.74 |
| 7 | instrumented_first | 1.87 | 295.49 | 1.44 | 276.72 | 1.28 | 267.70 |
| 8 | baseline_first | 1.45 | 308.61 | 1.35 | 281.52 | 1.26 | 267.62 |
| 9 | instrumented_first | 1.39 | 292.26 | 1.32 | 269.17 | 1.27 | 259.29 |

---

### 3.3 Statistical summary per session

Spread is defined explicitly as:
$$\text{Spread} = \frac{\max - \min}{\text{median}}$$

#### Compile-time overhead summary (N=5 each, medians reported)

| Scenario | Session | Baseline Median (min, max, spread) | Instrumented Median (min, max, spread) | Absolute Delta | Overhead Delta (%) |
|---|---|---|---|---|---|
| **Clean Build** | 1 | 9.157 s (8.790, 9.268, 5.2%) | 8.944 s (8.813, 9.009, 2.2%) | -0.213 s | -2.3% |
| | 2 | 8.824 s (7.524, 9.722, 24.9%) | 8.984 s (8.134, 9.420, 14.3%) | +0.160 s | +1.8% |
| | 3 | 7.626 s (7.570, 8.022, 5.9%) | 7.671 s (7.531, 7.774, 3.2%) | +0.045 s | +0.6% |
| **Repeat Build** | 1 | 0.038 s (0.036, 0.040, 10.5%) | 0.222 s (0.217, 0.223, 2.7%) | +0.184 s | +484.2% |
| | 2 | 0.034 s (0.033, 0.036, 8.8%) | 0.205 s (0.204, 0.207, 1.5%) | +0.171 s | +502.9% |
| | 3 | 0.035 s (0.034, 0.037, 8.6%) | 0.205 s (0.202, 0.208, 2.9%) | +0.170 s | +485.7% |
| **Incremental (App)** | 1 | 0.177 s (0.171, 0.179, 4.5%) | 0.362 s (0.355, 0.377, 6.1%) | +0.185 s | +104.5% |
| | 2 | 0.157 s (0.156, 0.168, 7.6%) | 0.339 s (0.325, 0.358, 9.7%) | +0.182 s | +115.9% |
| | 3 | 0.157 s (0.156, 0.163, 4.5%) | 0.329 s (0.323, 0.339, 4.9%) | +0.172 s | +109.6% |

#### Runtime overhead summary (N=10 each after discarded warm-up)

| Session | Baseline Median (min, max, spread) | Instrumented Median (min, max, spread) | Absolute Delta | Overhead Delta (%) |
|---|---|---|---|---|
| 1 | 1.405 ns (1.370, 1.870, 35.6%) | 289.930 ns (279.890, 308.610, 9.9%) | +288.525 ns/call | +20,535.6% |
| 2 | 1.335 ns (1.280, 1.440, 12.0%) | 271.175 ns (264.830, 281.520, 6.2%) | +269.840 ns/call | +20,212.7% |
| 3 | 1.370 ns (1.250, 1.530, 20.4%) | 262.965 ns (252.090, 267.740, 6.0%) | +261.595 ns/call | +19,094.5% |

#### Release binary size (Release profile)

Across all three sessions, the resulting release executables produced identical binary sizes:
- Baseline binary: 778,728 bytes
- Instrumented binary: 834,296 bytes
- Delta: +55,568 bytes (+7.1%)

---

## 4. Cross-Session Variation and Analysis

### 4.1 Cross-session variability

Comparing like quantities across the three session medians:

- **Clean build compile medians**:
  - Baseline medians: 7.626 s, 8.824 s, 9.157 s $\rightarrow$ cross-session spread: **17.4%**
  - Instrumented medians: 7.671 s, 8.944 s, 8.984 s $\rightarrow$ cross-session spread: **14.7%**
  - Observations: Absolute clean build durations varied by up to 1.5 s between sessions due to concurrent system load and I/O pressure. However, within each session, clean build overhead delta remained between **-2.3% and +0.6% / +1.8%**. Clean compilation overhead is effectively within the host machine's background noise envelope for this fixture.
- **Repeat and incremental compile deltas**:
  - Repeat build absolute delta across sessions: **+0.184 s, +0.171 s, +0.170 s** (170–184 ms).
  - Incremental app-edit absolute delta across sessions: **+0.185 s, +0.182 s, +0.172 s** (172–185 ms).
  - Observations: While percentage deltas appear large due to a sub-40 ms repeat baseline, the **absolute wall-time penalty** of the `cargo-instrument` invocation (running the CLI wrapper, JSON metadata pre-pass, ambient environment scrubbing, and candidate discovery) is consistent at **~170–185 ms** regardless of whether the build was a no-op or a single-file application modification.
- **Runtime per-call overhead**:
  - Baseline runtime medians: 1.34 ns, 1.37 ns, 1.41 ns $\rightarrow$ cross-session spread: **5.1%**
  - Instrumented runtime medians: 262.97 ns, 271.18 ns, 289.93 ns $\rightarrow$ cross-session spread: **9.9%**
  - Observations: Runtime per-call overhead falls within **+261.6 to +288.5 ns/call** (including counting-processor span handling). Discarding the initial warm-up prevented cold-cache anomalies from entering the sample distribution.

### 4.2 Comparison with the historical pilot (2026-10-07)

*Caution*: The sampling protocol changed between the October 7 pilot (fixed baseline-first order, N=5 runtime, no discarded warm-up) and this evaluation (alternating order, N=10 runtime, discarded warm-up). The experiments are related but not identical.

| Metric | October 7 Pilot (idle host run 3) | October 8 Evaluation (Session 2 / Session 3) | Protocol differences |
|---|---|---|---|
| Clean build overhead | +4.0% (+0.256 s) | +1.8% (+0.160 s) / +0.6% (+0.045 s) | Alternating order eliminates fixed baseline-first cache advantage |
| Repeat build delta | +0.150 s (+150 ms) | +0.171 s (+171 ms) / +0.170 s (+170 ms) | Consistent across repeat and incremental |
| Incremental build delta | +0.150 s (+150 ms) | +0.182 s (+182 ms) / +0.172 s (+172 ms) | Preserved separate target directory semantics |
| Runtime per-call delta | +249.34 ns/call | +269.84 ns/call / +261.60 ns/call | Discarded warm-up + N=10 runtime samples |
| Release binary growth | +55,560 bytes (+7.1%) | +55,568 bytes (+7.1%) | Consistent byte growth within 8 bytes |

---

## 5. Correctness and Validation Evidence

All three sessions verified expected instrumentation contracts:

1. **Native R-4 Emitter Route**:
   Captured from release build stderr and asserted in each session:
   ```text
   EMITTER: [cargo-instrument PID=... crate=bench_dep] selecting native R-4 emitter
   EMITTER: [cargo-instrument PID=... crate=bench_dep] transformed 20 candidates into mirror ...
   ```
2. **Workspace Application Exclusion**:
   Asserted that `bench_app` produced zero wrapper transform lines under `--with-dependencies`.
3. **Exact Span Assertions**:
   - Every instrumented runtime sample (and warm-up) asserted `span_count == 500000` completed dependency spans inside the application counting processor.
   - Every baseline runtime sample (and warm-up) asserted `span_count == 0`.
4. **Execution Status**:
   All three benchmark sessions exited with `0`.

---

## 6. Limitations

- **Bounded synchronous fixture only**: Only exercises a synchronous call loop with a counting processor. Does not exercise asynchronous tasks, Tokio task migrations, exporter flushes, or network transport.
- **Single host**: All three sessions were conducted as separate processes on one Linux host under realistic desktop load (loadavg 5.16–8.45). They reflect process session independence, not hardware or platform independence.
- **Timing composition**: Compile times reflect whole-command wall time (including cargo-instrument wrapper bootstrap, JSON metadata analysis, and cargo invocations). Internal substeps were not individually instrumented or subtracted.
- **No general performance claims**: These numbers represent pilot fixture measurements and do not establish production performance budgets or SLA guarantees.

---

## 7. Evidence Location and Verification

Raw evidence is preserved in [`evidence/phase3-overhead-2026-10-08/`](../../evidence/phase3-overhead-2026-10-08/):

| File | SHA-256 Checksum | Description |
|---|---|---|
| `env-manifest.txt` | `ec41c7460148f201e1e8bb6ccc4993d7d812bab1a4d9d95ed2bd754f48337d39` | Host environment, CPU/memory, toolchain, lockfile identity |
| `harness.patch` | `60f56738e95feee4bb597cc2a8cfb5648a74ba36c4ac0c065a913edd080bec0d` | Captured diff for `cargo-instrument/benches/bench_overhead.rs` |
| `run-bench.sh` | `7eb00b069bfa3ddf15dfed93242f2d9c9d8908e22858e5df3d262c5820e44b85` | Exact execution script with timestamp and loadavg logging |
| `session1.log` | `5407125fd48d87cacf6013466f87054bbcfd0aa2ce8cd840bd9e3a7b91314675` | Session 1 complete raw stdout/stderr |
| `session2.log` | `1e3b89f64127cf8f3c3f3e24619efd6f0d76bee7c56d0155c1acdfbee83c7bc1` | Session 2 complete raw stdout/stderr |
| `session3.log` | `38d92c461dddfbb05b1bd046aaf3357d0c25910ce59a514b5e8f5384a283d8af` | Session 3 complete raw stdout/stderr |

### How to rerun

```sh
# Verification of benchmark compilation (offline):
CARGO_NET_OFFLINE=true cargo bench -p cargo-instrument --bench bench_overhead --no-run

# Workflow regression tests:
cargo test -p cargo-instrument --test dependency_instrumentation_e2e_tests

# Full benchmark execution (outputs to target/phase3-overhead-rerun/ by default to preserve committed session logs):
./evidence/phase3-overhead-2026-10-08/run-bench.sh 1
```

---

## 8. Next Bounded Experiment

The synchronous evaluation protocol is now strengthened with alternating execution order, discarded warm-up, N=10 runtime samples, and cross-session replication.

The next planned experiment (Phase 3 roadmap section 3.3 / 3.6) is an **async workload with realistic work per call**, evaluating trace trees, futures suspension, and Tokio task lifecycle overhead.
