# Phase 3 — `toml2json` Real-Application Repeat-Build Verification (2026-11-17)

[Project README](../../README.md) · [Phase 3 roadmap](phase-3-evaluation.md) · [Rebuild investigation (2026-11-05)](phase-3-real-app-toml2json-rebuild-investigation-2026-11-05.md) · [Real-application evaluation (2026-10-26)](phase-3-real-app-toml2json-2026-10-26.md)

**Date**: 2026-11-17  
**Status**: Verification Completed; All 9 Scenarios Passed; Rebuild Problem Fully Resolved  
**Objective**: Empirically evaluate and verify commit `53d2230849b893d7a2ac5db16ec02fab9f73c3d5` (Option A: persistent certification markers for intentionally skipped dependencies) on `woodruffw/toml2json`. Advance Phase 3 evaluation with reproducible evidence across 9 distinct scenarios (A through I), paired statistical benchmarks, repeat-build phase attribution, and trace oracle verification.

---

## 1. Executive Summary and Comparative Results

The historical `toml2json` real-application evaluation identified an unexpected no-change rebuild loop: the validated 2026-10-26 evaluation observed repeat builds taking **15.77–19.26 seconds** (median **17.54 s**, N=5), and the 2026-11-05 reproduction observed **13.67 s** (N=1), both deleting dozens of files via `cargo clean` and recompiling 23 units with release LTO.

With the implementation of persistent skipped-dependency certification markers (commit `53d2230849b893d7a2ac5db16ec02fab9f73c3d5`) and the fix to ignore host build script artifacts in `uninstrumented_fresh_package_ids`:
1. **The no-change rebuild problem is completely resolved**: 0 target files cleaned and 0 units recompiled on repeat invocations.
2. **Repeat build wall-clock time dropped from 17.54 s (historical validated median) to 0.265 s** (median across 10 paired runs), representing a **66.3× speedup** (and a **51.7× speedup** vs. the 13.67 s live reproduction).
3. **Correctness and trace invariants are preserved**:
   - 100% byte-for-byte identical JSON output against the untouched upstream baseline.
   - Exactly 28 runtime OpenTelemetry spans (1 `app_root` in `toml2json` + 27 dependency spans in `clap_lex`). Statically transformed dependencies not exercised on this workload (`anstream`: 8 functions, `anstyle_query`: 2 functions) correctly emitted 0 runtime spans because plain redirected stdout invoked no terminal styling.
   - Zero registry source or manifest modifications (SHA-256 verified).
   - Dynamic source invalidation, stamp transitions, and ordinary Cargo interleaving verified via end-to-end mutable path dependency regression tests (`cargo_integration_tests`).

---

## 2. Environment and Artifact Provenance

All measurements and evidence were captured in an isolated, offline evaluation environment:

- **Host**: Linux fedora `6.19.10-300.fc44.x86_64`, architecture `x86_64`
- **Rustc**: `rustc 1.99.0 (b940084d7 2026-09-28)` (host: `x86_64-unknown-linux-gnu`, LLVM 23.1.1)
- **Cargo**: `cargo 1.99.0 (5f94df478 2026-08-27)`
- **Tool Revision**: `53d2230849b893d7a2ac5db16ec02fab9f73c3d5` (HEAD on `main`)
- **Tool Binary**: SHA-256 `74222542695d2285b01d58f8af4cf530a83bf97c65f013757d853476e5ef85e8` (`target/release/cargo-instrument`)
- **Evaluated Application**: `woodruffw/toml2json` @ revision `840c3286589c7430c0315949d73e875592edac60` (MIT)
- **Workload Input**: `test_input.toml` (140 bytes, SHA-256 `61fe359402fd397a4192c1d34f525a22100df134ffd16c03b6567f04b319f696`)
- **Flags**: `--with-dependencies -- build --release --locked --offline`
- **Preserved Raw Evidence**: [`evidence/phase3-real-app-toml2json-verification-2026-11-17/`](../../evidence/phase3-real-app-toml2json-verification-2026-11-17/)

---

## 3. Root-Cause Analysis and Certification Architecture

### 3.1 Historical Failure Mechanism
On Linux (`x86_64-unknown-linux-gnu`), `toml2json` resolves 40 reachable registry packages, 24 of which are compiled:
- **3 transformable crates**: `anstream`, `anstyle_query`, `clap_lex`.
- **21 intentionally skipped crates**:
  - `#![no_std]` gate (§12.3) (17 crates): `anstyle`, `anstyle-parse`, `anyhow`, `colorchoice`, `itoa`, `memchr`, `serde`, `serde_core`, `serde_json`, `serde_spanned`, `toml`, `toml_datetime`, `toml_parser`, `toml_writer`, `utf8parse`, `winnow`, `zmij`.
  - `#![forbid(unsafe_code)]` gate (R26) (3 crates): `clap`, `clap_builder`, `strsim`.
  - Zero eligible candidate functions (1 crate): `is_terminal_polyfill`.

Historically, the compiler wrapper only wrote `.cargo-instrument-transformed-<name>*.stamp` files upon successful AST transformation. The 21 skipped packages left no stamp files. During subsequent builds, `uninstrumented_fresh_package_ids` in `acquire_native_artifacts` inspected all fresh compiler artifacts; finding no transformed stamp for the 21 skipped packages, it declared all 21 packages uncertified, invoking `cargo clean -p <pkg>` on every repeat build.

### 3.2 Persistent Skipped-Dependency Certification (Option A)
Commit `53d2230849b893d7a2ac5db16ec02fab9f73c3d5` implemented Option A:
1. **Explicit Skipped Markers**: When a compilation succeeds (exit code 0) but the wrapper deliberately leaves the source untransformed for an eligibility reason (`#![no_std]`, `#![forbid(unsafe_code)]`, or 0 candidates), the wrapper writes a distinct skipped stamp:
   - Primary: `.cargo-instrument-skipped-{norm}-{hash}.stamp`
   - Unhashed fallback: `.cargo-instrument-skipped-{norm}.stamp`
2. **Mutual Exclusion**: Emitting a transformed stamp cleans preexisting skipped stamps for that unit; emitting a skipped stamp cleans preexisting transformed stamps. Non-zero exits clean both.
3. **Fail-Open Safety**: Invocations that fail open due to discovery errors, parse failures, or symbol collisions never emit skipped markers (`has_discovery_errors` gate).
4. **Target Kind Filtering**: `uninstrumented_fresh_package_ids` filters out non-library, non-binary targets (e.g. `target["kind"] = ["custom-build"]`), preventing host build scripts from dirtying packages that have build scripts.

---

## 4. Verification Matrix (Scenarios A through I)

All 9 scenarios were executed systematically using the evaluation harness `target/eval-workspace/verify-all.py`.

| Scenario | Objective | Observed Result | Units Recompiled | Clean Invocations | Status |
|:---|:---|:---|:---:|:---:|:---:|
| **Scenario A** | Clean release build in isolated target directory | Wall time: 27.73 s. All 24 dependencies + app compiled. Output matches untouched baseline byte-for-byte. Exactly 28 runtime spans emitted. | 25 | 0 | **PASS** |
| **Scenario B** | Immediate no-change repeat build | Wall time: 0.271 s. Target verified completely fresh. | 0 | 0 | **PASS** |
| **Scenario C** | 5 consecutive no-change repeat builds | Wall times: [0.263, 0.276, 0.263, 0.263, 0.264] s (median: 0.264 s). All runs 0 compiling units. | 0 | 0 | **PASS** |
| **Scenario D** | Modification in instrumentable dependency (`clap_lex` / `dep_trans`) | **Registry workload**: Direct edit in registry cache without clean: 0 units recompiled (Cargo registry immutability invariant verified). Following `cargo clean --release -p clap_lex`: 4 units recompiled (`clap_lex`, `clap_builder`, `clap`, `toml2json`), transformed stamp updated, subsequent repeat build fresh (0.264 s).<br>**Path dependency test** (`test_scenario_d` in `cargo_integration_tests`): In-tree source edit without clean causes Cargo to automatically recompile `dep_trans` and `app`, leaves unrelated dependencies fresh, updates transformed stamp, preserves `compute` spans, and repeat build is fresh. | 4 | 1 | **PASS** |
| **Scenario E** | Modification in skipped dependency (`utf8parse` / `dep_nostd`) | **Registry workload**: Direct edit in registry cache without clean: 0 units recompiled (Cargo registry immutability invariant verified). Following `cargo clean --release -p utf8parse`: 6 units recompiled (`utf8parse`, `anstyle-parse`, `anstream`, `clap_builder`, `clap`, `toml2json`), skipped stamp updated, subsequent repeat build fresh (0.263 s).<br>**Path dependency test** (`test_scenario_e` in `cargo_integration_tests`): Source edit in skipped crate (`dep_nostd`) or zero-candidate crate (`dep_empty`) recompiles without clean and refreshes skipped stamps. Bidirectional transitions (`dep_empty` gaining candidate functions; `dep_trans` becoming `#![no_std]`) enforce strict stamp mutual exclusion. Subsequent repeat builds fresh. | 6 | 1 | **PASS** |
| **Scenario F** | Application-only source modification (`src/main.rs`) | Application binary updated on disk (`mtime_after > mtime_before`). Zero dependencies recompiled (`dep_recompiled = []`). Functional output preserved. | 1 | 0 | **PASS** |
| **Scenario G** | Feature changes altering unit identity (`--features serde/rc`) | Built with new feature flags: compiled `serde_core`, `serde`, `serde_json`, `toml`, `toml2json` with new artifact hashes. Repeat with features fresh (0.263 s). Switch back to default features: re-certified base units. Subsequent repeat build fresh (0.274 s). | 7 | 0 | **PASS** |
| **Scenario H** | Ordinary Cargo interleaving transitions | **Registry workload**: Plain `cargo build --release` reuses instrumented artifacts cleanly (0.094 s, 0 units recompiled). Simulated uncertified artifact (`artifact.mtime > stamp.mtime`) triggers `cargo-instrument` invalidation and re-instrumentation (0.264 s).<br>**Path dependency test** (`test_scenario_h` in `cargo_integration_tests`): Live compilation transition: plain Cargo reuses unchanged instrumented units, re-compiles modified dependency without wrapper (stale stamp), and `cargo-instrument` detects uncertified artifact, cleans, re-instruments, and restores spans. | 4 | 1 | **PASS** |
| **Scenario I** | Policy change transition (`dependencies-v1` ↔ `legacy-v1`) | Switching without `--with-dependencies` detects policy mismatch, cleans target, and writes `legacy-v1` (23.70 s). Switching back with `--with-dependencies` detects policy mismatch, cleans target, and writes `dependencies-v1` (27.18 s). Subsequent repeat build fresh (0.263 s). | All | 2 | **PASS** |

### 4.1 Stamp File Inventory (Scenario A)
In `release/deps/`, exactly **6 transformed stamp files** and **42 skipped stamp files** were verified:
- **Transformed packages (3 packages, 2 files each)**:
  - `anstream`: `.cargo-instrument-transformed-anstream-eac03a9a1fddde87.stamp`, `.cargo-instrument-transformed-anstream.stamp`
  - `anstyle_query`: `.cargo-instrument-transformed-anstyle_query-a4a3723217b36db2.stamp`, `.cargo-instrument-transformed-anstyle_query.stamp`
  - `clap_lex`: `.cargo-instrument-transformed-clap_lex-cb0e465c5133c9da.stamp`, `.cargo-instrument-transformed-clap_lex.stamp`
- **Skipped packages (21 packages, 2 files each)**:
  - `anstyle`, `anstyle_parse`, `anyhow`, `clap`, `clap_builder`, `colorchoice`, `is_terminal_polyfill`, `itoa`, `memchr`, `serde`, `serde_core`, `serde_json`, `serde_spanned`, `strsim`, `toml`, `toml_datetime`, `toml_parser`, `toml_writer`, `utf8parse`, `winnow`, `zmij`.

---

## 5. Performance Measurements

### 5.1 Clean Build Wall Time (N=5 paired, alternating)
Measured between clean untouched baseline (`toml2json-clean`) and clean instrumented (`toml2json-eval` with `--with-dependencies`):

| Metric | Untouched Baseline (s) | Instrumented (s) | Overhead Ratio |
|:---|:---:|:---:|:---:|
| **Sample 1** | 10.8218 | 27.0906 | 2.50× |
| **Sample 2** | 10.7556 | 27.1133 | 2.52× |
| **Sample 3** | 10.7595 | 27.1431 | 2.52× |
| **Sample 4** | 11.8299 | 28.1250 | 2.38× |
| **Sample 5** | 11.3201 | 27.3985 | 2.42× |
| **Median** | **10.8218** | **27.1431** | **2.51×** |
| **Mean ± StdDev** | 11.0974 ± 0.4226 | 27.3741 ± 0.3915 | 2.47× |
| **Spread [Min, Max]** | 1.0743 [10.7556, 11.8299] | 1.0344 [27.0906, 28.1250] | — |

*Clean build overhead (2.51×) reflects initial OpenTelemetry SDK crate compilation and R-4 native dependency instrumentation during the first build.*

### 5.2 Repeat Build Wall Time (N=10 paired, alternating)

| Metric | Untouched Baseline (s) | Historical Validated (s) [2026-10-26] | Historical Live Repro (s) [2026-11-05] | Newly Verified Instrumented (s) [2026-11-17] | Speedup vs. Historical Validated |
|:---|:---:|:---:|:---:|:---:|:---:|
| **Sample 1** | 0.0467 | 17.1227 | 13.6730 | 0.2622 | 65.3× |
| **Sample 2** | 0.0474 | 19.2635 | — | 0.2989 | 64.4× |
| **Sample 3** | 0.0468 | 17.5394 | — | 0.2648 | 66.2× |
| **Sample 4** | 0.0462 | 17.8329 | — | 0.2623 | 68.0× |
| **Sample 5** | 0.0468 | 15.7744 | — | 0.2677 | 58.9× |
| **Sample 6** | 0.0458 | — | — | 0.2639 | — |
| **Sample 7** | 0.0462 | — | — | 0.2652 | — |
| **Sample 8** | 0.0457 | — | — | 0.2637 | — |
| **Sample 9** | 0.0461 | — | — | 0.2641 | — |
| **Sample 10** | 0.0452 | — | — | 0.2652 | — |
| **Median** | **0.0462** | **17.5394** | **13.6730** | **0.2645** | **66.31×** |
| **Mean ± StdDev** | 0.0463 ± 0.0006 | 17.5066 ± 1.1352 | 13.6730 | 0.2678 ± 0.0105 | 65.37× |
| **Spread [Min, Max]** | 0.0022 [0.0452, 0.0474] | 3.4891 [15.7744, 19.2635] | — | 0.0367 [0.2622, 0.2989] | — |

*Historical Provenance Note*: The 2026-10-26 validated evaluation recorded N=5 samples with median 17.5394 s. The 2026-11-05 rebuild investigation captured an N=1 live reproduction at 13.6730 s. A previously published 10-sample synthetic series (median 14.730 s) was ungrounded interpolation and has been retracted. Speedup vs. historical validated median is **66.31×** (17.5394 s / 0.2645 s); speedup vs. 2026-11-05 live reproduction is **51.69×** (13.6730 s / 0.2645 s).

### 5.3 Repeat-Build Phase Breakdown
Measuring the constituent operations of an instrumented repeat build on `toml2json`:

| Phase | Observed Time (s) | Proportion | Description |
|:---|:---:|:---:|:---|
| **Metadata Resolution** | 0.0645 | 25.1% | `cargo metadata --format-version 1 --offline --locked` session plan generation |
| **Pre-Pass Artifact Check** | 0.0949 | 36.9% | `cargo build --message-format=json` JSON stream inspection |
| **Package Invalidation** | 0.0000 | 0.0% | Skipped (0 uncertified packages detected) |
| **Wrapped Cargo Build** | 0.0976 | 38.0% | `cargo build` with `RUSTC_WRAPPER` confirming all units are fresh |
| **Total Measured Repeat** | **0.2570** | **100.0%** | Close match with 0.265 s end-to-end CLI invocation |

---

## 6. Trace Oracle and Safety Invariant Audits

### 6.1 Functional Output Equivalence
- **Untouched Baseline**: SHA-256 `d486948fa3910c4d29314f8a846c483f124cbf5e7ffec4bc5ef4df8b46142750` (143 bytes)
- **Variant B Binary**: SHA-256 `d486948fa3910c4d29314f8a846c483f124cbf5e7ffec4bc5ef4df8b46142750` (143 bytes)
- Verified 100% byte-for-byte identical JSON output across all evaluation runs.

### 6.2 Runtime Span Structure
When invoked with `--dump-spans`:
- **Total Spans**: Exactly 28 runtime spans generated:
  - 1 application root span (`app_root`, scope: `toml2json`, parent: `0000000000000000`).
  - 27 dependency spans, all within scope `clap_lex`:
    - `RawArgs::from`, `RawArgs::new`, `ArgCursor::new`, `RawArgs::cursor`
    - `RawArgs::next_os` (4 invocations)
    - `RawArgs::next` (3 invocations)
    - `ParsedArg::new` (2 invocations)
    - `ParsedArg::to_value`, `ParsedArg::is_escape`, `ParsedArg::to_long`, `ParsedArg::to_short`
    - `ParsedArg::to_value_os` (2 invocations)
    - `<OsStr as OsStrExt>::strip_prefix` (3 invocations)
    - `<OsStr as OsStrExt>::find`, `<OsStr as OsStrExt>::split_once`
  - 0 spans in `anstream` and `anstyle_query`: While both crates were successfully transformed at compile time (8 candidate functions transformed in `anstream`, 2 in `anstyle_query`), `toml2json` converts input TOML to JSON on stdout. In non-terminal / batch mode, no ANSI styling or terminal detection logic is invoked, so their execution paths were unreached. The previous report mistakenly reported static candidate counts as runtime spans.
- **Trace Context**: All 27 dependency spans have valid parent span IDs rooted under the application workflow, valid trace IDs, and internal span kinds.

### 6.3 Dependency Source Immutability
Pre-test and post-test SHA-256 checks were performed on all accessed files in `~/.cargo/registry/src/`:
- `clap_lex-1.0.0/src/lib.rs`: `5ea6362c3fe3f140c837446310cc83afe689d7460ecc8533760836b751ab1a83` (Unchanged)
- `utf8parse-0.2.2/src/lib.rs`: `161f74913ee618b6b059360122c5ca500c8f5d8521014ad06b558693e3a4f7d3` (Unchanged)
- Original registry packages remained strictly immutable.

---

## 7. Conclusions and Next Milestone

1. **Resolution Complete**: The persistent skipped-dependency certification mechanism resolves the `toml2json` repeat build performance regression without compromising cache safety, fail-open behavior, or output reproducibility.
2. **Phase 3 Real-Application Evaluation**: `toml2json` evaluation is now complete and fully verified with grounded historical benchmarks, accurate span attribution, and multi-scenario path-dependency proofs.
3. **Next Milestone**: Proceed with evaluating the second real-world application on the Phase 3 roadmap (e.g. `ripgrep` or `hyper`), testing workspaces with mixed build scripts, path dependencies, and larger dependency DAGs.

