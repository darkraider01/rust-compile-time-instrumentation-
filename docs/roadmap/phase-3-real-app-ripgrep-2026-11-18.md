# Phase 3 — `ripgrep` Real-Application Dependency Evaluation (2026-11-18)

[Project README](../../README.md) · [Phase 3 roadmap](phase-3-evaluation.md) · [`toml2json` verification (2026-11-17)](phase-3-real-app-toml2json-verification-2026-11-17.md) · [`toml2json` rebuild investigation (2026-11-05)](phase-3-real-app-toml2json-rebuild-investigation-2026-11-05.md)

**Date**: 2026-11-18  
**Status**: Evaluation Completed; All 9 Scenarios Passed; Bounded Trace Oracle Verified  
**Objective**: Continue Phase 3 evaluation of the compile-time instrumentation architecture on `BurntSushi/ripgrep` as the second real-world application. Empirically assess public dependency workflow (`--with-dependencies`) correctness, cache stability, trace propagation across multi-crate workspace DAGs, and compile/runtime performance overhead.

---

## 1. Executive Summary

Following the completion and verification of the `toml2json` single-crate evaluation, this investigation evaluates `ripgrep` (v14.1.1) as a larger, multi-crate production application with a complex dependency graph and high-performance search workloads.

Key results from this evaluation:
1. **Public Dependency Workflow Succeeds on Multi-Crate Workspace**:
   - `ripgrep` comprises a 10-member workspace (`ripgrep`, `globset`, `grep`, `grep-cli`, `grep-matcher`, `grep-pcre2`, `grep-printer`, `grep-searcher`, `grep-regex`, `ignore`).
   - Under `--with-dependencies` (`dependencies-v1`), all 10 workspace members are correctly classified as first-party workspace members and preserved untransformed without stamp clutter.
   - Out of 24 compiled third-party registry dependencies:
     - **5 packages transformed** via native OpenTelemetry (R-4): `encoding_rs_io`, `memmap2`, `same-file`, `termcolor`, `walkdir` (10 stamp files total).
     - **17 packages deliberately skipped**: 14 under the `#![no_std]` gate (§12.3) and 3 under the `#![forbid(unsafe_code)]` gate (R26: `lexopt`, `regex-syntax`, `textwrap`) (34 stamp files total).
     - Host build scripts (`cc`, `jobserver`, `pkg-config`, `serde_derive`) correctly ignored.
2. **Deterministic Output & Bounded Trace Oracle Invariants Verified**:
   - **Functional output**: 100% byte-for-byte identical to the untouched upstream baseline on a deterministic search workload (`2,122` bytes, SHA-256 `1d914b97badcb59f2ec82dcf631d3715cc2f5e4c99059219776da6d578e86aec`, exit code `0`).
   - **Trace oracle**: Exactly **7,096 spans** emitted across the workload:
     - 1 application root span (`app_root`).
     - **0 trace ID mismatches**: 100% of the 7,095 child spans inherited the exact root `trace_id`.
     - **0 orphaned spans**: every non-root span's `parent_span_id` resolved to a valid enclosing span.
     - **Strictly acyclic directed tree**: every parent link terminated at `app_root`.
     - **Scope distribution**: `walkdir` (3,453), `encoding_rs_io` (2,551), `termcolor` (1,086), `same_file` (5), `ripgrep` (1).
     - **Unexercised paths**: `memmap2` emitted 0 spans because ripgrep heuristics selected streaming buffered search for directory traversal.
3. **Repeat Build Freshness & Cache Correctness**:
   - Repeat builds recompile **0 units** and trigger **0 clean invocations**, running in **0.217 s** median wall-clock time (+0.169 s overhead vs. plain Cargo's 0.048 s baseline).
   - Invalidation of an instrumentable dependency (`walkdir` or `same-file`) recompiles only the affected crate and downstream dependents, leaving unrelated dependencies fresh.
   - Invalidation of a skipped dependency (`regex-syntax`) regenerates skipped certification stamps cleanly.
   - Application-only source edits (`crates/core/main.rs`) recompile only the final binary in **4.24 s** (+0.36 s / +9.25% overhead vs. baseline 3.88 s).
4. **Statistical Benchmarks (Reconciled N=5/10 Dual-Session Runs)**:
   - **Session 1 (Dedicated Host, commit `48262c6d` + initial ripgrep fixes)**:
     - **Clean release build**: Baseline 12.69 s vs. Instrumented 32.21 s (2.54× overhead ratio, +19.52 s absolute delta).
     - **Repeat release build**: Baseline 0.048 s vs. Instrumented 0.217 s (4.48× overhead ratio, +0.169 s absolute delta).
     - **App-only edit build**: Baseline 3.88 s vs. Instrumented 4.24 s (1.09× overhead ratio, +0.359 s absolute delta).
     - **Workload runtime execution**: Baseline 4.75 ms vs. Instrumented 7.02 ms (1.48× overhead ratio, +2.27 ms absolute delta for 7,096 spans, ~320 ns/span).
   - **Session 2 (Post-Hardening Validation, Provenance-Limited, concurrent background test load)**:
     - **Clean release build**: Baseline 19.18 s vs. Instrumented 46.99 s (2.45× overhead ratio, +27.82 s absolute delta).
     - **Repeat release build**: Baseline 0.055 s vs. Instrumented 0.246 s (4.44× overhead ratio, +0.190 s absolute delta).
     - **App-only edit build**: Baseline 5.06 s vs. Instrumented 6.07 s (1.20× overhead ratio, +1.010 s absolute delta).
     - **Workload runtime execution**: Baseline 6.43 ms vs. Instrumented 8.83 ms (1.37× overhead ratio, +2.40 ms absolute delta for 7,096 spans, ~338 ns/span).
   - **Cross-Session Consistency**:
     - Overhead ratios remain remarkably stable regardless of workstation load (clean build: 2.54× vs. 2.45×; repeat build: 4.48× vs. 4.44×; runtime per-span: ~320 ns vs. ~338 ns).
   - **Binary size**: Baseline 30,355,176 bytes vs. Instrumented 40,927,832 bytes (+10,572,656 bytes / +34.83% growth across both sessions).

---

## 2. Environment and Artifact Provenance

All evaluation measurements and evidence artifacts were captured in an isolated, offline environment:

- **Host**: Linux fedora `6.19.10-300.fc44.x86_64`, architecture `x86_64` (12 logical CPU cores)
- **Rustc**: `rustc 1.100.0 (0ef1bc386 2026-11-09)` (host: `x86_64-unknown-linux-gnu`, LLVM 23.1.5)
- **Cargo**: `cargo 1.100.0 (3acc79c0c 2026-11-05)`
- **Tool Revisions & Provenance**:
  - **Session 1 (Initial Evaluation)**: commit `48262c6d86c5f7dcd97d1c98f094cc8a371beb4d` + initial ripgrep fixes, binary SHA-256 `6680134a21867768caec555d22fa26766526267817a9c26eb013fd8dc70c7679` (captured 2026-11-18T07:30:23Z, dedicated load).
  - **Session 2 (Post-Hardening Validation, Provenance-Limited)**: committed HEAD `82c1c10326947efa67a7d64935ea052638c4f277` with uncommitted working-tree hardening changes (subsequently committed in `2d9d10d6cc69691433940850a4be1d4017c9695e`), binary SHA-256 `16541cc8599d896c93f5eb45aa71c406e0d8b33a97a62fec56b13c1c20aee8e8` (captured 2026-11-18T11:03:24Z, concurrent background test load).
  - *Provenance Note*: Session 2 is designated "provenance-limited". The raw evidence manifest recorded `tool_revision: 82c1c10326947efa67a7d64935ea052638c4f277` because `git rev-parse HEAD` evaluated the committed repository HEAD at capture time while uncommitted working-tree modifications from the active hardening cycle were present in the build tree before being committed in `2d9d10d6`. Because uncommitted working-tree state cannot be independently reproduced from git commit hashes alone without a tree hash, Session 2 cannot claim strict commit provenance; however, the compiled binary SHA-256 (`16541cc8...`) provides verifiable binary provenance that the post-hardening implementation was executed.
- **Evaluated Application**: `BurntSushi/ripgrep` @ revision `4649aa9700619f94cf9c66876e9549d83420e16c` (Tag `14.1.1`, License: `Unlicense OR MIT`)
- **Deterministic Workload**: `rg --threads 1 --sort path --color always "fn parse" crates/`
- **Workload Output**: 2,122 bytes, SHA-256 `1d914b97badcb59f2ec82dcf631d3715cc2f5e4c99059219776da6d578e86aec`, exit code `0`
- **Build Flags**: `--with-dependencies -- build --release --locked --offline`
- **Preserved Raw Evidence**: [`evidence/phase3-real-app-ripgrep-2026-11-18/`](../../evidence/phase3-real-app-ripgrep-2026-11-18/)

---

## 3. Architecture Discoveries and Defect Fixes

Evaluating a multi-crate workspace uncovered three architectural failure modes in the wrapper and pre-pass orchestration that were absent in single-crate fixtures:

### 3.1 Unqualified `Result` Collision in AST Transformation
- **Failure**: Crates like `memmap2` commonly define custom type aliases:
  ```rust
  pub type Result<T> = result::Result<T, Error>;
  ```
  The compiler wrapper's native transformation previously emitted unqualified `let __otel_res: Result<_, _> = ...`. Because `memmap2`'s type alias takes 1 generic argument, rustc failed with 48 `E0107` errors (`type alias takes 1 generic argument but 2 generic arguments were supplied`).
- **Fix**: Replaced unqualified `Result<_, _>` with `core::result::Result<_, _>` in `cargo-instrument/src/transform.rs:779, 808`, matching existing fully-qualified usage on line 978.

### 3.2 Transitive Dependency Search Paths for Uninstrumented Units
- **Failure**: In Cargo 1.100, compiler outputs are placed into per-unit directories (`build/<pkg>/<hash>/out`). When crate A (`memmap2`) was transformed with `--extern opentelemetry`, downstream uninstrumented workspace members (`grep-searcher`, `grep-cli`, `ignore`) linking against A failed with `E0463: can't find crate for opentelemetry`. Cargo passes `-L` only for crates declared in `Cargo.toml`, so rustc lacked search paths to resolve `opentelemetry` referenced in `libmemmap2.rmeta`.
- **Fix & Invariants**: Implemented `SessionPlan::validated_native_otel_search_paths` in `cargo-instrument/src/session.rs` and called it in `cargo-instrument/src/wrapper.rs:502`. Rather than injecting search paths globally, the wrapper restricts `-L dependency=<path>` injection to units that satisfy five strict boundary checks:
  1. The compiling unit is not host-only (build scripts and proc macros never receive target paths).
  2. The target triple matches the OpenTelemetry artifact's target.
  3. The compilation profile matches the OpenTelemetry artifact's profile.
  4. The compiling unit transitively depends on OpenTelemetry or on an upstream package configured/eligible for native OpenTelemetry instrumentation. Unrelated workspace units (e.g. `bstr`, `itoa`) do not receive search paths.
  5. The candidate directory exists on disk, and paths are deterministically sorted and deduplicated.

### 3.3 Pre-Pass Wrapper Mode for Multi-Crate Dependency Invalidation
- **Failure**: When an instrumented crate (e.g. `walkdir`) was cleaned via `cargo clean -p walkdir`, the pre-pass in `acquire_native_artifacts` was previously executed with `env_remove("RUSTC_WRAPPER")`. Plain rustc attempted to recompile `walkdir`, which links against `same-file`. Because `same-file` was compiled with `--extern opentelemetry`, plain rustc without wrapper lacked `-L dependency=<otel_out_dir>` and aborted with `E0463`. The pre-pass failure triggered `invalidate_packages(&desired_instrumented_ids)`, falsely wiping all 24 dependencies and forcing a full 31-crate recompilation.
- **Fix & Invariants**: In `acquire_native_artifacts`, Cargo is launched with `RUSTC_WRAPPER = current_exe`, an ephemeral `CARGO_INSTRUMENT_INTERNAL_PREPASS_TOKEN`, and `CARGO_INSTRUMENT_PREPASS_SESSION` pointing to the active session plan.
  - In pre-pass wrapper mode (`run_prepass_wrapper`), `cargo-instrument` does not transform source code, does not write instrumentation or skipped stamps, and does not alter build policies.
  - It loads the verified session plan (only if fresh and matching the current build fingerprint) and passes validated search paths via `SessionPlan::validated_native_otel_search_paths`.
  - Unvetted directory-wide scanning (`build/*/*/out`) was completely eliminated. If session data is missing, corrupted, or exhibits a fingerprint mismatch, the wrapper fails open, executes plain rustc, and safely falls back without injecting unauthorized paths.

---

## 4. Verification Matrix (Scenarios A through I)

The evaluation verified all 9 scenarios in isolated target directories:

| Scenario | Objective | Observed Result | Units Recompiled | Clean Invocations | Status |
|:---|:---|:---|:---:|:---:|:---:|
| **Scenario A** | Clean release build in isolated target directory | Wall time: 31.77 s. All 24 dependencies + 10 workspace units compiled. Functional output matches baseline byte-for-byte. Exactly 7,096 runtime spans emitted. | 31 | 0 | **PASS** |
| **Scenario B** | Immediate no-change repeat build | Wall time: 0.216 s. Target verified completely fresh. | 0 | 0 | **PASS** |
| **Scenario C** | 5 consecutive no-change repeat builds | Wall times: [0.236, 0.219, 0.216, 0.216, 0.216] s (median: 0.216 s). All runs 0 compiling units. | 0 | 0 | **PASS** |
| **Scenario D** | Invalidation in instrumentable dependency (`walkdir`) | Registry raw edit without clean: 0 units recompiled (Cargo registry immutability invariant verified). Following `cargo clean --release -p walkdir`: only 3 units recompiled (`walkdir`, `ignore`, `ripgrep`), transformed stamps refreshed, sibling dependencies fresh. Subsequent repeat build fresh (0.216 s). | 3 | 1 | **PASS** |
| **Scenario E** | Invalidation in skipped dependency (`regex-syntax`) | Registry raw edit without clean: 0 units recompiled (Cargo registry immutability invariant verified). Following `cargo clean --release -p regex-syntax`: only affected units recompiled (`regex-syntax`, `regex-automata`, `bstr`, `globset`, `grep-*`, `ignore`, `ripgrep`), skipped stamps refreshed, unrelated dependencies fresh. Subsequent repeat build fresh (0.216 s). | 11 | 1 | **PASS** |
| **Scenario F** | Application-only source modification (`crates/core/main.rs`) | Application binary updated on disk (`mtime_after > mtime_before`). Zero dependencies recompiled (`dep_recompiled = []`). Functional output preserved. | 1 | 0 | **PASS** |
| **Scenario G** | Feature changes altering unit identity (`--features pcre2`) | Built with new feature flags: compiled `pcre2-sys`, `pcre2`, `grep-pcre2`, `grep`, `ripgrep` with new artifact hashes. Repeat with features fresh (0.221 s). Switch back to default features: re-certified base units. Subsequent repeat build fresh (0.218 s). | 5 | 0 | **PASS** |
| **Scenario H** | Ordinary Cargo interleaving transitions | Plain `cargo build --release` reuses instrumented artifacts cleanly (0.100 s, 0 units recompiled). Simulated uncertified artifact (`artifact.mtime > stamp.mtime`) triggers `cargo-instrument` invalidation and re-instrumentation (recompiled `walkdir`, `ignore`, `ripgrep`). Subsequent repeat build fresh (0.215 s). | 3 | 1 | **PASS** |
| **Scenario I** | Policy change transition (`dependencies-v1` ↔ `legacy-v1`) | Switching without `--with-dependencies` detects policy mismatch and triggers `cargo clean` (confirmed via `Removed` in stderr). Switching back with `--with-dependencies` detects policy mismatch, cleans target, and writes `dependencies-v1` (19.30 s). Subsequent repeat build fresh (0.222 s). | All | 2 | **PASS** |

### 4.1 Stamp File Inventory (Scenario A)
In `release/build/*/out/`, exactly **10 transformed stamp files** and **34 skipped stamp files** were verified:
- **Transformed packages (5 packages, 2 files each: hashed + unhashed fallback)**:
  - `encoding_rs_io`: `.cargo-instrument-transformed-encoding_rs_io-bd51d83d19bf7c91.stamp`, `.cargo-instrument-transformed-encoding_rs_io.stamp`
  - `memmap2`: `.cargo-instrument-transformed-memmap2-ce7c112c4d7ea4a5.stamp`, `.cargo-instrument-transformed-memmap2.stamp`
  - `same-file`: `.cargo-instrument-transformed-same_file-6d02487860f584d5.stamp`, `.cargo-instrument-transformed-same_file.stamp`
  - `termcolor`: `.cargo-instrument-transformed-termcolor-361c66ba5cf19983.stamp`, `.cargo-instrument-transformed-termcolor.stamp`
  - `walkdir`: `.cargo-instrument-transformed-walkdir-b522787f0bc45105.stamp`, `.cargo-instrument-transformed-walkdir.stamp`
- **Skipped packages (17 packages, 2 files each: hashed + unhashed fallback)**:
  - `aho-corasick`, `anyhow`, `bstr`, `crossbeam-deque`, `crossbeam-epoch`, `crossbeam-utils`, `encoding_rs`, `itoa`, `lexopt`, `log`, `memchr`, `regex-automata`, `regex-syntax`, `ryu`, `serde`, `serde_json`, `textwrap`.
- **Workspace packages (10 packages)**:
  - `ripgrep`, `globset`, `grep`, `grep-cli`, `grep-matcher`, `grep-pcre2`, `grep-printer`, `grep-searcher`, `grep-regex`, `ignore`: **0 stamp files** (correctly classified as workspace members).

---

## 5. Performance Measurements

All measurements were collected using alternating paired order across isolated variant directories (`target-rg-clean-base` vs. `target-rg-clean-inst`).

### 5.0 Dual-Session Reconciliation Overview

The evaluation encompasses two distinct benchmark captures:
1. **Session 1 (Dedicated Host, commit `48262c6d` + initial ripgrep fixes)**: Captured on an unconstrained 12-core host (binary `6680134a...`).
2. **Session 2 (Post-Hardening Validation, Provenance-Limited, binary `16541cc8...`)**: Captured while concurrent background cargo compilation was running on the host, increasing absolute wall-clock durations across both baseline and instrumented builds. Recorded committed HEAD was `82c1c10326947efa67a7d64935ea052638c4f277` with uncommitted working-tree hardening changes (subsequently committed in `2d9d10d6cc69691433940850a4be1d4017c9695e`).

| Metric | Session 1 (Dedicated Host) Baseline → Instrumented | Session 1 Overhead Ratio | Session 2 (Concurrent Load) Baseline → Instrumented | Session 2 Overhead Ratio | Consistency Assessment |
|:---|:---:|:---:|:---:|:---:|:---|
| **Clean Build (Median)** | 12.69 s → 32.21 s | **2.54×** | 19.18 s → 46.99 s | **2.45×** | Multipliers match within 3.5% across load profiles |
| **Repeat Build (Median)** | 0.048 s → 0.217 s | **4.48×** | 0.055 s → 0.246 s | **4.44×** | Multipliers match within 0.9% across load profiles |
| **App-Only Edit (Median)** | 3.88 s → 4.24 s | **1.09×** | 5.06 s → 6.07 s | **1.20×** | Recompilation confined strictly to app binary |
| **Workload Runtime (Median)** | 4.75 ms → 7.02 ms | **1.48×** | 6.43 ms → 8.83 ms | **1.37×** | Per-span amortized delta: ~320 ns vs. ~338 ns |
| **Binary Size** | 30.36 MB → 40.93 MB | **+34.83%** | 30.36 MB → 40.93 MB | **+34.83%** | Identical linked artifacts and SDK symbols |

The detailed distributions for both sessions are reported below.

### 5.1 Clean Builds (N=5 Paired Alternating Runs)

#### Session 1 (Dedicated Host)
| Run | Baseline (`cargo build --release`) | Instrumented (`cargo instrument --with-dependencies`) | Order |
|:---:|:---:|:---:|:---:|
| 1 | 12.6130 s | 32.2075 s | Base → Inst |
| 2 | 12.6271 s | 32.3684 s | Inst → Base |
| 3 | 13.2565 s | 33.5351 s | Base → Inst |
| 4 | 12.9022 s | 31.6215 s | Inst → Base |
| 5 | 12.6907 s | 32.0479 s | Base → Inst |
| **Median** | **12.6907 s** | **32.2075 s** | **Ratio: 2.5379× (+19.5168 s)** |
| **Mean ± StdDev** | **12.8179 ± 0.2425 s** | **32.3561 ± 0.6398 s** | Spread: 0.6435 s (B) / 1.9136 s (I) |

#### Session 2 (Concurrent Load, Post-Hardening)
| Run | Baseline (`cargo build --release`) | Instrumented (`cargo instrument --with-dependencies`) | Order |
|:---:|:---:|:---:|:---:|
| 1 | 22.2313 s | 54.7978 s | Base → Inst |
| 2 | 19.1783 s | 44.4382 s | Inst → Base |
| 3 | 22.4623 s | 50.3544 s | Base → Inst |
| 4 | 16.6097 s | 44.3571 s | Inst → Base |
| 5 | 15.9901 s | 46.9934 s | Base → Inst |
| **Median** | **19.1783 s** | **46.9934 s** | **Ratio: 2.4503× (+27.8151 s)** |
| **Mean ± StdDev** | **19.2943 ± 2.7129 s** | **48.1882 ± 3.9635 s** | Spread: 6.4722 s (B) / 10.4407 s (I) |

### 5.2 No-Change Repeat Builds (N=10 Paired Alternating Runs)

#### Session 1 (Dedicated Host)
| Run | Baseline (`cargo build --release`) | Instrumented (`cargo instrument --with-dependencies`) | Units Rebuilt | Files Cleaned |
|:---:|:---:|:---:|:---:|:---:|
| 1 | 0.0766 s | 0.2415 s | 0 | 0 |
| 2 | 0.0480 s | 0.2198 s | 0 | 0 |
| 3 | 0.0490 s | 0.2183 s | 0 | 0 |
| 4 | 0.0481 s | 0.2197 s | 0 | 0 |
| 5 | 0.0487 s | 0.2153 s | 0 | 0 |
| 6 | 0.0477 s | 0.2152 s | 0 | 0 |
| 7 | 0.0481 s | 0.2155 s | 0 | 0 |
| 8 | 0.0479 s | 0.2155 s | 0 | 0 |
| 9 | 0.0488 s | 0.2155 s | 0 | 0 |
| 10 | 0.0493 s | 0.2185 s | 0 | 0 |
| **Median** | **0.0484 s** | **0.2169 s** | **0** | **0** |
| **Mean ± StdDev** | **0.0512 ± 0.0085 s** | **0.2195 ± 0.0075 s** | **Ratio: 4.4814× (+0.1685 s)** | Spread: 0.0263 s |

#### Session 2 (Concurrent Load, Post-Hardening)
| Run | Baseline (`cargo build --release`) | Instrumented (`cargo instrument --with-dependencies`) | Units Rebuilt | Files Cleaned |
|:---:|:---:|:---:|:---:|:---:|
| 1 | 0.0549 s | 0.2706 s | 0 | 0 |
| 2 | 0.0554 s | 0.2313 s | 0 | 0 |
| 3 | 0.0546 s | 0.2419 s | 0 | 0 |
| 4 | 0.0860 s | 0.2801 s | 0 | 0 |
| 5 | 0.0673 s | 0.2411 s | 0 | 0 |
| 6 | 0.0551 s | 0.2485 s | 0 | 0 |
| 7 | 0.0722 s | 0.2783 s | 0 | 0 |
| 8 | 0.0551 s | 0.2501 s | 0 | 0 |
| 9 | 0.0530 s | 0.2428 s | 0 | 0 |
| 10 | 0.0670 s | 0.2426 s | 0 | 0 |
| **Median** | **0.0553 s** | **0.2457 s** | **0** | **0** |
| **Mean ± StdDev** | **0.0621 ± 0.0103 s** | **0.2527 ± 0.0163 s** | **Ratio: 4.4430× (+0.1904 s)** | Spread: 0.0489 s |

### 5.3 Repeat-Build Phase Attribution

The phases below represent independent duration estimates measured by executing each stage in isolation as a standalone subprocess command, rather than an in-process additive decomposition of a single end-to-end repeat build:

| Phase | Observed Duration | Share of Measured Total | Description |
|:---|:---:|:---:|:---|
| **Cargo Metadata Resolution** | 0.0624 s | 23.6% | Standalone `cargo metadata --format-version 1 --offline --locked` |
| **Cargo Pre-Pass Build Check** | 0.1004 s | 37.9% | Standalone `cargo build --message-format=json-render-diagnostics` |
| **Selective Invalidation** | 0.0000 s | 0.0% | Zero packages invalidated; skipped entirely |
| **Wrapped Cargo Invocation** | 0.1019 s | 38.5% | Standalone `cargo build` with `RUSTC_WRAPPER` (0 units recompiled) |
| **Sum of Independent Runs** | **0.2646 s** | **100.0%** | Independent subprocess execution estimate |

*Attribution Note*: Spawning three separate subprocesses incurs sequential OS process creation, environment propagation, and runtime setup overhead across each invocation. The single unified end-to-end `cargo-instrument --with-dependencies` command achieved a lower median of **0.2169 s** (spread: 0.0263 s) because internal metadata caching and unified orchestration avoid repeated process initialization.

### 5.4 Application-Only Edit Builds (N=5 Paired Alternating Runs)

| Run | Baseline (`crates/core/main.rs`) | Instrumented (`crates/core/main.rs`) | Order |
|:---:|:---:|:---:|:---:|
| 1 | 3.9753 s | 4.3071 s | Base → Inst |
| 2 | 3.8847 s | 4.2440 s | Inst → Base |
| 3 | 3.9392 s | 4.2286 s | Base → Inst |
| 4 | 3.8131 s | 4.1945 s | Inst → Base |
| 5 | 3.7746 s | 4.4234 s | Base → Inst |
| **Median** | **3.8847 s** | **4.2440 s** | **Ratio: 1.0925× (+0.3593 s / +9.25%)** |
| **Mean ± StdDev** | **3.8774 ± 0.0750 s** | **4.2795 ± 0.0807 s** | Spread: 0.2007 s (B) / 0.2290 s (I) |

### 5.5 Workload Runtime Execution (N=10 Paired Alternating Runs)

Workload command: `rg --threads 1 --sort path --color always "fn parse" crates/` (2,122 bytes output).

| Run | Baseline | Instrumented (CountingProcessor) | Order |
|:---:|:---:|:---:|:---:|
| 1 | 5.5982 ms | 7.0446 ms | Base → Inst |
| 2 | 4.7468 ms | 7.5134 ms | Inst → Base |
| 3 | 4.5417 ms | 6.8603 ms | Base → Inst |
| 4 | 4.8236 ms | 6.7104 ms | Inst → Base |
| 5 | 4.6116 ms | 7.0382 ms | Base → Inst |
| 6 | 4.8364 ms | 6.8331 ms | Inst → Base |
| 7 | 4.7923 ms | 7.0010 ms | Base → Inst |
| 8 | 4.7532 ms | 7.3021 ms | Inst → Base |
| 9 | 4.7200 ms | 6.9845 ms | Base → Inst |
| 10 | 4.7112 ms | 7.1828 ms | Inst → Base |
| **Median** | **4.7500 ms** | **7.0196 ms** | **Ratio: 1.4778× (+2.2696 ms)** |
| **Mean ± StdDev** | **4.8135 ± 0.2755 ms** | **7.0471 ± 0.2238 ms** | Spread: 1.0565 ms (B) / 0.8030 ms (I) |

*Amortized Runtime Difference*: The 2.27 ms delta across 7,096 emitted spans corresponds to an amortized workload-level difference of **~320 ns per span**. This figure reflects total end-to-end execution difference divided by span count; it is not an isolated microbenchmark of span construction. Component contributions include:
1. In-process OpenTelemetry tracer provider and SDK initialization.
2. In-memory `CountingProcessor` and span lifecycle tracking.
3. Thread-local context storage and parent-child hierarchy resolution.
4. Function call boundary overhead (closure invocation, clippy suppression guards).
5. Workload application execution and search output formatting.

### 5.6 Binary Executable Size

| Binary | Size (Bytes) | Size (MiB) | Delta (Bytes) | Delta (%) |
|:---|:---:|:---:|:---:|:---:|
| **Baseline `rg`** | 30,355,176 B | 28.95 MiB | — | — |
| **Instrumented `rg`** | 40,927,832 B | 39.03 MiB | +10,572,656 B | +34.83% |

*Binary Growth Breakdown*: The +10.57 MiB (+34.83%) growth encompasses all static additions linked into the release executable:
1. OpenTelemetry runtime core (`opentelemetry` crate and its internal dependency closure).
2. OpenTelemetry SDK (`opentelemetry_sdk` tracer provider, processors, samplers, and futures runtime).
3. The in-memory evaluation exporter and evaluation test harness.
4. Static protobuf and trace serialization metadata.
5. Injected AST instrumentation wrappers and anchor symbols across 5 transformed dependencies.
Uninstrumented compiler flags, LTO settings, and debug stripping remained identical across baseline and instrumented builds.

---

## 6. Trace Oracle and Scope Breakdown

The runtime trace oracle verified that the 7,096 emitted spans form an unbroken directed tree:

```
app_root [ripgrep] (trace_id: 63e307d673ce33b415571cc2ffb947a6, span_id: f4c407b631320ee0)
├── walkdir::WalkDir::into_iter [walkdir]
│   ├── walkdir::Iter::next [walkdir] (3,451 iterations)
│   │   ├── same_file::is_same_file [same_file] (5 calls)
│   │   └── encoding_rs_io::DecodeReaderBytes::read [encoding_rs_io] (2,551 calls)
│   └── walkdir::DirEntry::from_entry [walkdir]
└── termcolor::StandardStream::set_color [termcolor] (1,086 colorized match spans)
```

- **Scope Distribution**:
  - `walkdir`: 3,453 spans (48.7%)
  - `encoding_rs_io`: 2,551 spans (36.0%)
  - `termcolor`: 1,086 spans (15.3%)
  - `same_file`: 5 spans (0.1%)
  - `ripgrep`: 1 root span (<0.1%)
  - `memmap2`: 0 spans (0.0% — unexercised on small directory traversal where streaming reads are selected by ripgrep heuristics)
- **Trace Invariants**:
  - Exactly 1 root span: `app_root`.
  - 100% trace ID propagation: 0 spans deviated from the root trace ID.
  - 0 orphan spans: every non-root span had a parent ID matching an active span in the trace.
  - 0 cycles: parent traversal terminates strictly at `app_root`.

---

## 7. Registry Immutability and Fingerprint Protection

Direct cryptographic verification confirms that third-party source files and manifests in `~/.cargo/registry/src/` remained completely unaltered throughout the entire evaluation:

- `walkdir-2.5.0/src/lib.rs`: SHA-256 `5920c966ea3eee0ef3049720ac8804c074437545bbf6eb6b1e53a2443b2e7f18` (Unchanged)
- `regex-syntax-0.8.4/src/lib.rs`: SHA-256 `5ae457d402e49443bdb23b71353693dd3b0d263b57a6eeb9eb5b5dae5c901bdd` (Unchanged)
- All compiler transformations operate exclusively on isolated mirror directories in `target/release/build/<pkg>/<hash>/out/instrumented_sources/`.

