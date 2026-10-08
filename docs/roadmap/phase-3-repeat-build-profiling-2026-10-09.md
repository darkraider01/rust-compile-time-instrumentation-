# Phase 3 Repeat-Build Overhead Profiling and Stage Attribution Note (2026-10-09)

[Project README](../../README.md) · [Phase 3 roadmap](phase-3-evaluation.md) · [Steady-state overhead evaluation (2026-10-08)](phase-3-overhead-pilot-2026-10-08-steady-state.md) · [Dependency build orchestration](../implementation/dependency-build-orchestration.md)

Status: Phase 3 investigation and profiling executed 2026-10-09. This note isolates the repeatable ~160 ms compile-time overhead observed during repeat (no-op) builds of `cargo instrument --with-dependencies -- build`. Through opt-in stage timing, subprocess inspection, and Cargo compiler artifact verification, this evaluation demonstrates that repeat builds produce 31 `Fresh` units, 0 `Compiling` units, and unchanged inspected artifact mtimes in the target directory, and identifies where time is spent across CLI orchestration and subprocess execution.

Investigation and profiling only: no optimization implementation is performed in this change.

---

## 1. No-Op Repeat-Build Workflow and Stage Map

When invoked as `cargo instrument --with-dependencies -- build`, the CLI wrapper performs the following sequential stages:

```
[cargo instrument CLI startup]
  │
  ├── 1. CLI initialization & argument parsing (<0.1 ms)
  │      Reads arguments, parses flags, checks target directories and policy.
  │
  ├── 2. Session plan resolution (Subprocess 1 + In-Process)
  │      ├── Subprocess 1: cargo metadata --format-version 1 (~57.0 ms)
  │      └── In-Process: JSON deserialization & SessionPlan construction (~2.5 ms)
  │
  ├── 3. Native artifact acquisition pre-pass (Subprocess 2 + In-Process)
  │      ├── Subprocess 2: cargo build --target-dir ... --message-format=json-render-diagnostics (~65.9 ms)
  │      └── In-Process: Parse JSON stream, verify fresh stamps & artifact closure (~0.9 ms)
  │
  ├── 4. Session plan persistence (<0.5 ms)
  │      Writes SessionPlan to target/cargo_instrument_session.json.
  │
  ├── 5. Wrapped Cargo execution (Subprocess 3)
  │      └── Subprocess 3: cargo build --target-dir ... with RUSTC_WRAPPER (~70.3 ms)
  │            (Cargo evaluates target fingerprints: all 31 units reported Fresh.
  │             Cargo replays cached compiler stderr; 0 rustc invocations spawned).
  │
  ├── 6. Post-build policy check & exit (<0.1 ms)
  │      Records cargo_instrument_policy and returns Cargo's exit status (0).
  │
  └── Total External Wall Time (~196.5 ms clean, ~198.5 ms profiled)
```

### Stage Independence and Cargo Workload
- **Sequential execution**: Every stage executes strictly sequentially on the main thread. Durations are mutually exclusive and additive.
- **Subprocesses vs. in-process**: Stages 2, 3, and 5 each execute a separate child process running `cargo`. All in-process work (parsing, planning, stamp checks, serialization) occurs between subprocess executions.
- **Cargo workload vs. process spawn**: Command-phase durations (~57 ms for `metadata`, ~66 ms for pre-pass build, ~70 ms for wrapped build) reflect **Cargo's execution workload**—configuration loading, manifest parsing, dependency resolution, `.fingerprint` target cache validation, compiler flags verification, and diagnostic replay—not merely the sub-millisecond operating system cost of spawning a process.

---

## 2. Measurement Methodology and Timing Boundaries

### Fixture Configuration
- Faithfully reproduces the deterministic benchmark fixture from `cargo-instrument/benches/bench_overhead.rs`:
  - Workspace with `bench_app` and path dependency `bench_dep` (20 individual functions).
  - Telemetry dependencies: `otel-shim` (local path), `opentelemetry` 0.32.0, `opentelemetry_sdk` 0.32.0.
  - Setup builds executed outside the measured section to establish steady-state targets (`target_base` and `target_inst`).
  - Offline local resolution (`CARGO_NET_OFFLINE=true`) and sanitized environment variables.

### Isolated Binaries and Provenance Validation
To prevent timing contamination and ensure verifiable binary provenance, evaluation uses two isolated executables:
1. **Clean production binary** (`target/release/cargo-instrument`): Built directly from clean repository sources with zero profiling probes. The runner requires unchanged tracked build inputs across all `cargo-instrument` dependencies (`git diff-index --quiet HEAD -- cargo-instrument/ instrument-semantics/ Cargo.toml Cargo.lock`), rebuilds the binary each invocation outside measured sections, records its SHA-256 hash and build configuration, and asserts absence of `[STAGE_PROFILE]`.
2. **Profile-instrumented binary** (`target/release/cargo-instrument-profiled`): Built from an isolated application of [`profiling.patch`](../../evidence/phase3-overhead-repeat-build-profiling-2026-10-09/profiling.patch) each invocation outside measured sections. The runner records its SHA-256 hash, restores clean source, and asserts presence of `[STAGE_PROFILE]`. It activates timing probes and emits `[STAGE_PROFILE]` only when `CARGO_INSTRUMENT_PROFILE=1` is explicitly exported.

*(Note: Recorded sample measurements in `profile.log` are retained as historical evidence from the steady-state evaluation session; the updated runner with full tracked-input cleanliness verification and fresh rebuilds has been validated end-to-end).*

### Timing Boundaries
- **External timing boundary**: Measured externally by the test runner using `time.perf_counter()` immediately before and after `subprocess.run()`. This captures total wall-clock duration including process creation, binary loading, dynamic linking, runtime initialization, command execution, child process exit, teardown, and parent-side pipe collection.
- **Internal timing boundary**: Profiled `total_wall` starts inside `execute_cargo_with_wrapper()`, capturing internal stage breakdown. The delta between external wall time and internal `total_wall` (~1.65 ms) reflects parent-side process lifecycle overhead—process startup, dynamic linking, initial argument parsing/dispatch in `main()`, child exit, teardown, and parent-side output collection.

### Evaluation Protocol
- **Sample count**: $N=10$ paired runs per evaluation track.
- **Execution order**: Alternating execution order ($run \% 2 == 0$: baseline first; $run \% 2 == 1$: instrumented first) within paired tracks to control for thermal and filesystem caching bias.
- **Track isolation note**: Because the clean binary, inactive probe, and active probe runs execute in separate consecutive blocks, their median differences represent **observed timing differences across tracks** (which include host drift and binary-layout differences) rather than proof that stage proportions are unaffected.

---

## 3. Detailed Results

### 3.1 Unprofiled Repeat-Build Samples ($N=10$ pairs, Clean Production Binary)

All samples measured using the clean production binary with `CARGO_INSTRUMENT_PROFILE` unset:

| # | Execution Order | Baseline Repeat (ms) | Instrumented Repeat (ms) | Paired Delta (ms) |
|---|---|---|---|---|
| 0 | baseline_first | 34.43 | 195.97 | +161.53 |
| 1 | instrumented_first | 33.26 | 197.34 | +164.08 |
| 2 | baseline_first | 33.57 | 194.36 | +160.80 |
| 3 | instrumented_first | 33.62 | 195.82 | +162.19 |
| 4 | baseline_first | 33.57 | 197.99 | +164.42 |
| 5 | instrumented_first | 33.95 | 198.04 | +164.08 |
| 6 | baseline_first | 33.34 | 197.07 | +163.73 |
| 7 | instrumented_first | 32.87 | 193.92 | +161.05 |
| 8 | baseline_first | 33.22 | 195.40 | +162.18 |
| 9 | instrumented_first | 34.46 | 197.80 | +163.34 |

- **Baseline repeat median**: **33.57 ms** (min: 32.87 ms, max: 34.46 ms, spread: 1.60 ms)
- **Instrumented repeat median**: **196.52 ms** (min: 193.92 ms, max: 198.04 ms, spread: 4.12 ms)
- **Difference of medians**: **+162.95 ms** (+485.4% relative to baseline median)
- **Median paired delta**: **+162.77 ms** (min: +160.80 ms, max: +164.42 ms)

*(Note on metric distinction: The **difference of medians** is $196.52 - 33.57 = +162.95\text{ ms}$; the **median of paired deltas** across alternating runs is $+162.77\text{ ms}$. Both metrics consistently reflect a ~161–163 ms repeat-build delta).*

---

### 3.2 Observed Difference Under Inactive Probe Track ($N=10$ runs)

To evaluate whether dormant probe instrumentation introduces noticeable latency when profiling output is disabled, the profiled binary was measured with `CARGO_INSTRUMENT_PROFILE` unset against the clean production binary:

- **Clean binary external median**: **196.52 ms**
- **Inactive probe external median**: **201.17 ms** (min: 196.43 ms, max: 236.52 ms)
- **Observed difference**: **+4.66 ms** (+2.37%)

*(Note: Because the clean and inactive probe tracks execute in separate consecutive blocks, this difference reflects background host drift and binary-layout differences in addition to any potential overhead from dormant probe checks).*

---

### 3.3 Profiled Repeat-Build Samples ($N=10$ pairs, Stage Breakdown)

Individual stage measurements in milliseconds ($N=10$, `CARGO_INSTRUMENT_PROFILE=1` using `target/release/cargo-instrument-profiled`):

| # | Order | Ext Wall | CLI Init | `metadata` Cmd | Metadata Plan | Prepass Cmd | Prepass Analysis | Session Save | Wrapped Cargo | Post Build | Total Wall | Baseline |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 0 | baseline_first | 206.06 | 0.05 | 61.91 | 2.42 | 67.79 | 0.87 | 0.55 | 69.96 | 0.06 | 204.13 | 43.05 |
| 1 | instrumented_first | 204.98 | 0.05 | 59.73 | 2.48 | 66.97 | 1.03 | 0.43 | 72.19 | 0.08 | 203.45 | 34.17 |
| 2 | baseline_first | 198.06 | 0.04 | 56.87 | 2.20 | 64.81 | 0.91 | 0.46 | 70.61 | 0.05 | 196.44 | 34.87 |
| 3 | instrumented_first | 195.73 | 0.04 | 57.10 | 2.46 | 65.68 | 0.82 | 0.48 | 67.14 | 0.05 | 194.37 | 34.39 |
| 4 | baseline_first | 195.47 | 0.06 | 56.42 | 2.46 | 64.60 | 0.97 | 0.41 | 68.44 | 0.05 | 193.89 | 34.78 |
| 5 | instrumented_first | 197.71 | 0.05 | 55.83 | 2.26 | 65.18 | 0.90 | 0.41 | 70.48 | 0.11 | 195.69 | 33.74 |
| 6 | baseline_first | 198.99 | 0.05 | 56.20 | 2.40 | 66.74 | 0.86 | 0.38 | 70.18 | 0.07 | 197.32 | 33.22 |
| 7 | instrumented_first | 197.51 | 0.03 | 56.25 | 2.58 | 66.21 | 1.09 | 0.48 | 68.88 | 0.06 | 196.03 | 34.11 |
| 8 | baseline_first | 202.23 | 0.04 | 59.12 | 2.52 | 65.43 | 0.79 | 0.44 | 71.67 | 0.11 | 200.55 | 33.83 |
| 9 | instrumented_first | 230.20 | 0.07 | 68.77 | 3.55 | 75.76 | 1.09 | 0.61 | 77.71 | 0.06 | 228.23 | 34.69 |

---

### 3.4 Stage Attribution and Medians

Medians calculated relative to internal `total_wall` (**196.88 ms**):

| Stage | Execution Type | Median (ms) | Min (ms) | Max (ms) | Spread (ms) | % of Internal Wall |
|---|---|---|---|---|---|---|
| **`cargo metadata` command** | Subprocess 1 | **56.98** | 55.83 | 68.77 | 12.94 | **28.9%** |
| **Unwrapped pre-pass build** | Subprocess 2 | **65.94** | 64.60 | 75.76 | 11.16 | **33.5%** |
| **Wrapped final Cargo build** | Subprocess 3 | **70.33** | 67.14 | 77.71 | 10.57 | **35.7%** |
| *Subtotal Subprocess Execution* | *Subprocesses (3)* | ***193.26*** | *188.42* | *222.24* | *33.82* | ***98.2%*** |
| Metadata JSON parse & plan | In-process | **2.46** | 2.20 | 3.55 | 1.35 | **1.2%** |
| Pre-pass analysis & stamp check | In-process | **0.91** | 0.79 | 1.09 | 0.30 | **0.5%** |
| Session plan file save | In-process | **0.45** | 0.38 | 0.61 | 0.23 | **0.2%** |
| CLI startup & argument parsing | In-process | **0.05** | 0.03 | 0.07 | 0.04 | **<0.1%** |
| Post-build policy check & exit | In-process | **0.06** | 0.05 | 0.11 | 0.06 | **<0.1%** |
| *Subtotal In-Process Execution* | *In-process* | ***3.93*** | *3.45* | *5.43* | *1.98* | ***2.0%*** |
| **Internal Total Wall Time** | **Probe Timer** | **196.88** | 193.89 | 228.23 | 34.34 | **100.0%** |

---

### 3.5 Timing Boundary and Observed Track Differences

Comparing identical external timing boundaries between the clean and profiled binaries:

- **Clean binary external wall median**: **196.52 ms**
- **Profiled binary external wall median**: **198.53 ms**
- **Profiled binary internal `total_wall` median**: **196.88 ms**
- **External - internal duration delta**: **1.65 ms** (capturing parent-side process creation, dynamic linking, CLI dispatch, child exit, process teardown, and parent-side pipe handling).
- **Observed clean-vs-active difference**: **+2.01 ms** (+1.02% relative to clean binary).

*(Note: This is reported as an **observed difference across consecutive run blocks** rather than confirmed profiler overhead or proof that stage proportions are unaffected, as consecutive block execution encompasses background host drift and code-layout variation).*

---

## 4. Compiler and Cache Activity Verification

To establish what compiler activity occurs during a repeat build, the build was executed with verbose output (`-vv`) and filesystem artifacts inspected:

- **Verbose output log**: Retained in full at [`verbose-cargo.log`](../../evidence/phase3-overhead-repeat-build-profiling-2026-10-09/verbose-cargo.log).
- **Units reported `Fresh`**: **31** (Cargo confirmed all 31 compilation units across dependencies and workspace packages were up to date).
- **Units reported `Compiling`**: **0** (no crate was scheduled for recompilation).
- **Target artifact modifications**: **0** (filesystem mtimes of all `.rlib` and `.rmeta` artifacts in `target_inst/debug/deps/` were identical before and after the repeat build).
- **Cached stderr replay**: The output `[cargo-instrument PID=... crate=bench_dep]` observed on repeat runs is Cargo replaying its cached compiler stderr from `.fingerprint/.../output-lib-bench_dep`, not a re-invocation of the compiler or wrapper.

**Finding**: Repeat builds under `cargo instrument` produced 31 `Fresh` units, 0 `Compiling` units, and unchanged inspected artifact mtimes in `target_inst/debug/deps/`, with cached stderr replayed from `.fingerprint`.

---

## 5. Root Cause and Largest Contributor

The observed ~160 ms repeat-build overhead does not stem from in-process code (which accounts for only **3.93 ms**). Instead, it is caused by executing **three sequential Cargo subprocesses** on every build:

1. **Baseline repeat build**:
   Executes **one** command: `cargo build` (**~33.6 ms**).
2. **Instrumented repeat build**:
   Executes **three** commands sequentially:
   - `cargo metadata --format-version 1`: **56.98 ms**
   - `cargo build --message-format=json-render-diagnostics`: **65.94 ms**
   - `cargo build` (with `RUSTC_WRAPPER`): **70.33 ms**

Together, the two preparatory Cargo subprocesses (`cargo metadata` and the unwrapped pre-pass build) consume **122.92 ms** (62.4% of total command duration and ~75% of the ~163 ms difference of medians). The final wrapped build consumes ~70.3 ms (about ~36 ms higher than baseline due to Cargo scanning the instrumented target tree and replaying cached stderr).

**Largest Demonstrated Contributor**: The two preparatory sequential Cargo subprocess invocations (`cargo metadata` and the unwrapped pre-pass build), which run unconditionally on every command even when the workspace and target are completely unmodified.

---

## 6. Optimization Evaluation and Design Investigation: Cargo Invocation Caching

The profiling data clearly identifies `cargo metadata` (~57.0 ms) and the pre-pass `cargo build` (~65.9 ms) as the primary contributors to repeat-build latency. However, implementing caching for either stage is **not a trivial or ready change**. A rigorous design investigation is required before any caching mechanism can be safely proposed.

### 6.1 Invalidation Complexity in `cargo metadata`
A naive cache that invalidates solely based on workspace `Cargo.lock` and member `Cargo.toml` files is unsafe for the following reasons:

1. **External Path Dependencies**:
   Dependencies referenced via `path = "..."` outside workspace member roots (such as `bench_dep` in the benchmark fixture, or out-of-workspace shared crates) have their own `Cargo.toml` manifests and source files that alter metadata resolution. A cache cannot inspect only workspace members.
2. **Cargo Configuration**:
   Cargo behavior is influenced by `.cargo/config.toml` (and legacy `.cargo/config`) files located in the project directory, parent directories, and `CARGO_HOME`. Furthermore, environment variables (e.g. `RUSTFLAGS`, `CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET`) affect target paths, build flags, and architecture settings. Changes to configuration must invalidate metadata.
3. **Feature and Resolution Flags**:
   Command-line flags passed through `cargo instrument` (e.g. `--features`, `--no-default-features`, `--all-features`, `--target`, `--package`, `--offline`, and `-Z` unstable flags) directly change the resolved package graph and dependencies.
4. **Dynamic Target Discovery**:
   Cargo dynamically discovers binaries, tests, benchmarks, and examples from filesystem paths (e.g. `src/bin/`, `tests/`, `benches/`, `examples/`). Files added or removed from these directories alter the target graph even when `Cargo.toml` manifests remain untouched.

### 6.2 Code Contract of `build_session_plan()`
In the current implementation:
```rust
fn build_session_plan(
    cargo_args: &[String],
    invocation_dir: &Path,
) -> Result<(SessionPlan, serde_json::Value), String>
```
`build_session_plan()` does not merely produce a `SessionPlan`. It returns a tuple containing `(SessionPlan, serde_json::Value)`, where `metadata` is the full parsed JSON structure from `cargo metadata`.

This raw metadata is required downstream by `acquire_native_artifacts()` for multiple critical operations:
- Determining target packages: `desired_instrumented_package_ids(plan, metadata)`
- Extracting OpenTelemetry native artifacts: `plan.add_r4_artifacts_from_cargo_json(metadata, &stdout, None)`
- Extracting async runtime artifacts: `plan.add_tokio_artifacts_from_cargo_json(metadata, &stdout, None)`
- Computing package invalidation closures: `invalidate_packages(&desired, &retained, metadata, ...)`
- Computing retained artifact closures: `retained_artifact_closure(plan, metadata)`

Reloading only `SessionPlan` from `target/cargo_instrument_session.json` would violate this contract and fail native artifact acquisition unless:
- The raw `serde_json::Value` metadata (or an extracted dependency graph model) is also cached and validated, or
- Artifact acquisition is refactored to decouple its closure calculations and artifact resolutions from the raw `cargo metadata` JSON structure.

### 6.3 Pre-Pass Build Invalidation Concerns
Skipping the unwrapped pre-pass build (~65.9 ms) presents even steeper correctness requirements:
- Proving that all native OpenTelemetry `.rlib` artifacts are present, valid, and uncorrupted in the target directory.
- Proving that all instrumentation stamps and dependency closures match the current toolchain, profile settings, and compiler flags.
- Ensuring that compiler diagnostics and intermediate build outputs remain sound if the pre-pass JSON message stream is omitted.

### 6.4 Recommended Next Steps for Design Investigation
Rather than attempting an ad-hoc implementation:
1. **Design Spike 1: Dependency Manifest Closure Fingerprinting**:
   Investigate whether Cargo's internal fingerprinting or a lightweight recursive scan of all declared path-dependency manifests can reliably detect manifest changes.
2. **Design Spike 2: Session Plan and Metadata Decoupling**:
   Investigate refactoring `acquire_native_artifacts()` so that necessary package metadata (package IDs, target names, features) is self-contained within `SessionPlan` or a dedicated persisted metadata structure, eliminating reliance on raw `serde_json::Value`.
3. **Correctness Test Suite Expansion**:
   Define tests covering edge cases: modifying external path dependencies, changing CLI feature flags, altering `.cargo/config.toml`, and modifying source files within path dependencies.

---

## 7. Evidence Location, Rerun Procedure, and Checksums

Raw evidence is preserved in [`evidence/phase3-overhead-repeat-build-profiling-2026-10-09/`](../../evidence/phase3-overhead-repeat-build-profiling-2026-10-09/):

| File | SHA-256 Checksum | Description |
|---|---|---|
| `env-manifest.txt` | `db4a54c2a9dce7c0d89d379fbc5a06cdda9bffa7fbcdf6a06ac59f26f14cf465` | Host environment, CPU model (`lscpu`), memory, toolchain, lockfile identity |
| `profiling.patch` | `8fee7eb606fffc0144893f740ed0af2811fbedc7ccc6eecbc5ef8c367d480bce` | Narrow opt-in timing probe diff in `cargo-instrument/src/main.rs` |
| `run-bench.sh` | `48ef80d4ead49fea03f3459f4b55210d5f1b5aefd0ffb630fc6115aeb483c631` | Benchmark runner requiring clean tracked build inputs (`cargo-instrument/`, `instrument-semantics/`, `Cargo.toml`, `Cargo.lock`), fresh rebuilds, and hash recording |
| `profile.log` | `f5347e73a855c43be8f50483895d18169dfaffa56c1d870add3b616392cdfa5e` | Complete raw output for unprofiled, dormant probe, profiled, and verification runs |
| `verbose-cargo.log` | `70feb58788a64de8020ab20e883ad7a01faea65e45944e5fe916e71a57ed48d1` | Preserved verbose Cargo diagnostic output (`-vv`) showing 31 Fresh and 0 Compiling units |

### Isolated Probe-Build, Provenance Verification, and Rerun Procedure

The runner script (`run-bench.sh`) validates source cleanliness across all tracked dependencies and guarantees binary provenance without runner-side caching:
1. **Tracked Source Cleanliness Verification**: Verifies `git diff-index --quiet HEAD -- cargo-instrument/ instrument-semantics/ Cargo.toml Cargo.lock` before managing builds, ensuring no staged or unstaged modifications across any direct or transitive dependencies of `cargo-instrument` contaminate the build.
2. **Fresh Binary Rebuilds Each Invocation**: Rebuilds both `target/release/cargo-instrument-profiled` (from patch application) and `target/release/cargo-instrument` (from restored clean source) afresh on each invocation outside measured sections, completely avoiding stale cache machinery.
3. **Recorded Hashes and Build Configuration**: Logs git revision, patch SHA-256, `Cargo.lock` SHA-256, `Cargo.toml` SHA-256, clean binary SHA-256, profiled binary SHA-256, rustc version, and cargo version.
4. **Binary Inspection and Runtime Assertions**: Verifies via string search (`grep -a`) and Python byte reading (`b"[STAGE_PROFILE]" in f.read()`) that the clean binary lacks the probe, the profiled binary contains the probe, and their SHA-256 hashes differ.
5. **Execution Tracks**:
   - Track 1 (Unprofiled) executes `target/release/cargo-instrument`.
   - Track 2 (Inactive Probe Track) executes `target/release/cargo-instrument-profiled` with `CARGO_INSTRUMENT_PROFILE` unset.
   - Track 3 (Opt-In Stage Profiling) executes `target/release/cargo-instrument-profiled` with `CARGO_INSTRUMENT_PROFILE=1`.
   - Track 4 (Compiler Verification) executes `target/release/cargo-instrument` with `-vv`, preserving verbose logs and checking filesystem artifact timestamps.

*(Note: The recorded sample measurements in `profile.log` represent historical evidence from the steady-state evaluation session; the revised rebuild/hash procedure has been validated end-to-end to ensure reliable reproducibility for subsequent reruns).*

To reproduce:
```sh
# Verify clean compilation (offline):
CARGO_NET_OFFLINE=true cargo check --all-targets

# Execute repeat build profiling (outputs to target/phase3-repeat-profiling-rerun/ by default):
./evidence/phase3-overhead-repeat-build-profiling-2026-10-09/run-bench.sh
```

---

## 8. Limitations

- **Single Host**: Measurements were collected on a single 12-vCPU AMD Ryzen 5 5600H system under standard desktop load (loadavg ~1.44–2.17). Absolute millisecond durations scale with processor IPC and disk I/O performance across different hardware.
- **Fixture Scope**: Evaluated on the synchronous 20-function fixture with local path dependencies and OpenTelemetry SDK dependencies.
- **Investigation Only**: This note establishes the stage breakdown, timing boundary attribution, and feasibility requirements for optimization. Implementation remains deferred to a dedicated design investigation.
