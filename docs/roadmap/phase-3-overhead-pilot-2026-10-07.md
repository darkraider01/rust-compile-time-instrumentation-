# Phase 3 Pilot — First Overhead Benchmark Result (2026-10-07)

[Project README](../../README.md) · [Phase 3 roadmap](phase-3-evaluation.md) · [2026-10-05 baseline](phase-3-coverage-baseline.md)

Status: first executed result from the existing [overhead benchmark](../../cargo-instrument/benches/bench_overhead.rs), run 2026-10-07 on Linux. Two successful runs are recorded: run 2 on a quiet desktop and run 3 on an idle host (desktop applications closed), plus one failed first attempt (section 2). This is a **pilot** (N=5 samples, fixed variant order, one deterministic synchronous fixture); it does not complete evaluation-protocol sections 3.2–3.4. The first attempt failed and exposed a release-profile route-selection defect (section 2); the minimal fix is in the working tree at the time of writing, uncommitted. The October 5 baseline note is unchanged and remains the record of that date.

## 1. What was measured

One command runs everything: `cargo bench -p cargo-instrument --bench bench_overhead` (harness = false), fixture generated into a fresh temp workspace per run.

| Dimension | Configuration |
| --- | --- |
| Variants | Baseline `cargo build` vs instrumented `cargo instrument --with-dependencies -- build` (policy `dependencies-v1`), separate target directory per variant per run |
| Compile scenarios | Clean ×5 (empty variant target dir), repeat/no-op ×5 (after untimed setup build), app-edit incremental ×5 (`main.rs` comment appended; untimed edit) |
| Runtime | Release binaries; 100,000 iterations × 5 `#[inline(never)]` dependency calls = 500,000 calls per sample ×5 samples per variant |
| Telemetry | Same telemetry dependencies and setup in **both** arms: app manifest always carries `otel-shim` + `opentelemetry`/`opentelemetry_sdk` 0.32; both binaries run `otel_shim::init()` and build an `SdkTracerProvider` with a counting `SpanProcessor` (no exporter) **before** the timed loop. Baseline expects/asserts 0 spans; instrumented asserts exactly 500,000 completed `compute_step_*` spans (assert outside the timed loop; the per-span counting itself — name check + atomic increment — is inside) |
| Emitter verification | Release instrumented build's stderr is captured; the bench asserts `crate=bench_dep] selecting native R-4 emitter` and asserts **no** `crate=bench_app` wrapper activity, and prints every `EMITTER:` line |
| Env hygiene | 15 ambient variables scrubbed from every child (`CARGO_INSTRUMENT_*` incl. the `CARGO_INSTRUMENT_ACTIVE` recursion guard, `INSTRUMENT_DEBUG`, `RUSTC_WRAPPER`, fault-injection hooks); `CARGO_NET_OFFLINE=true` supplied by the runner (harness itself has no `--offline`/`--locked` flags); lockfile generated once, untimed |
| Worker count / profile | Default Cargo jobs (12-core host); debug profile for compile scenarios, release for runtime/size |
| Fixture | Generated workspace: `bench_dep` (20 sync functions, excluded path dependency), `bench_app` (workspace member) |

## 2. Run 1 failure: release build selected Tier-2 instead of native R-4

Run 1 (09:49:14–09:51:24Z) collected all 15 compile sample pairs and then **failed with exit 101** at the bench's emitter assertion: the release instrumented build selected `Tier-2 C-ABI emitter`. All 16 debug-build instrumented invocations in the same run had selected native R-4. Raw log retained (section "Evidence location").

Diagnosis (reproduced standalone with full stderr; the bench's own stderr filter hides the reason):

- The wrapper's `r4_native_otel_artifact_for` reported `profile R4Profile { opt_level: "3", debug_assertions: true, overflow_checks: true }` against Cargo's authoritative artifact record `debug_assertions: false, overflow_checks: false` → mismatch → the documented S11 fail-open to Tier-2 (ADR-007-013).
- Root cause: `R4Profile::default()` documented "Release/custom profiles carry explicit `-C` values". Observed on this toolchain: `cargo build --release -v` passes `-C opt-level=3` and **no** `-C debug-assertions`/`-C overflow-checks`, because cargo omits flag values equal to rustc's own defaults — and rustc enables `debug-assertions` (and, by its default, `overflow-checks`) **only at opt-level 0** (verified empirically at opt-level 0/1/2/3, `--print cfg` plus runtime overflow probes; the decoupled combinations were corrected in review, section 2). Release argv is therefore just as flag-less as dev argv; the fixed dev-profile default made release units permanently unmatchable, so dependency builds on `--release` could never select native R-4. Cargo does pass explicit flags when a profile deviates (observed `-C debug-assertions=on` under `CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true`), so explicit values remain authoritative.
- Scope: this is an **instrumentation defect** (route selection), not a harness or environment failure. Fail-open behavior itself is per-ADR and was not changed.

Minimal fix (working tree, `cargo-instrument/src/session.rs` only):

1. `rustc_profile()` now parses `opt-level` first and resolves *absent* flags the way rustc does: codegen `debug-assertions` from the explicit `-C` value or the parsed opt-level default (on iff opt-level == "0"), then `overflow-checks` from the explicit `-C` value or that *codegen* `debug-assertions` value (its documented default — not from the opt-level independently). A forced `--cfg debug_assertions` still overrides the recorded `debug_assertions` field but does not feed `overflow-checks` (it changes `cfg!()` without changing codegen). Explicit `-C` values remain authoritative.
2. `R4Profile::default()` comment corrected (behavior kept: dev-profile sentinel).
3. New unit test `test_rustc_profile_resolves_absent_flags_against_rustc_defaults` pinning the empty (dev) argv, the exact observed release argv shape, explicit-flag precedence, explicit-off at opt-level 0, both mixed combinations from review (`opt-level=3` + `debug-assertions=on` → overflow checks on; `opt-level=0` + `debug-assertions=off` → off; plus explicit `-C overflow-checks=off` beating the derived default), and the forced-`--cfg` separation case (recorded field true, overflow checks false).

Review correction (same day): an initial version derived absent `overflow-checks` from the opt-level directly. Review flagged it with rustc 1.99 probes for both mixed combinations; the probes were reproduced here (optimized + `debug-assertions=on` panics on overflow, unoptimized + `debug-assertions=off` wraps) and the parser was corrected before the note was finalized. After the correction: fmt, clippy (`-D warnings`), and the full test set above were re-run (all pass), and the standalone release repro was rebuilt with the corrected binary (native R-4 selected, exit 0). A follow-up review then found the `--cfg debug_assertions` override was merged in before the `overflow-checks` derivation; the parser now separates codegen `debug-assertions` from the cfg override, verified with the reviewer's rustc 1.99 probe (`-Copt-level=3 -Cdebug-assertions=off --cfg=debug_assertions -Aexplicit_builtin_cfgs_in_flags` reports `cfg!(debug_assertions) == true` while runtime overflow wraps; reproduced here), pinned by the regression case above. After this follow-up correction: fmt, clippy (`-D warnings`), `--lib` (17), `native_artifact_compatibility_tests` (10), `tokio_spawn_tests` (21), `discovery_tests` (12), and the whole `dependency_instrumentation_e2e_tests` file (6, including the `phase3_coverage_baseline` oracle) were re-run — all pass — and the release repro was rebuilt with the final binary (native R-4, exit 0; stderr retained as `fix_r4_release_after_cfg_correction.stderr`). Neither correction changes resolution for the standard dev/release profiles used by the benchmark (there `debug-assertions` and `overflow-checks` are equal and no `--cfg` override is involved), so the recorded measurements remain valid without a rerun.

Verification of the fix (all executed 2026-10-07): `cargo fmt --check` ✓; `cargo clippy --all-targets -- -D warnings` ✓; `cargo test --lib` 17 passed (includes the new test); `native_artifact_compatibility_tests` 10 passed (release-vs-dev mismatch still fails open, `test_h2_profile_mismatch_fails_open` unaffected); `tokio_spawn_tests` 21 passed; `discovery_tests` 12 passed; `dependency_instrumentation_e2e_tests` whole file 6 passed (27.75 s); `phase3_coverage_baseline` re-run passed (8.53 s); standalone release repro now selects `native R-4 emitter`, injects `--extern opentelemetry=...rlib`, and links with exit 0 (stderr retained). The pre-existing release-mismatch tests all pair release argv against a *dev* artifact and remain Err by `opt_level` mismatch.

The first-party pinned-nightly apply test was **not** rerun: the fix touches only dependency-route profile matching, not first-party HIR application.

## 3. Environment and versions (both successful runs)

| Item | Value |
| --- | --- |
| Revision | `b525171c35f0790a307c31a0b4d988b001b38dda`, branch `main`; dirty only by the section-2 fix (`session.rs` + test) |
| OS / host | Linux fedora 6.19.10-300.fc44.x86_64, `x86_64-unknown-linux-gnu`, 12 cores, 18 GiB RAM |
| Toolchain | `rustc 1.99.0 (b940084d7 2026-09-28)`, `cargo 1.99.0` (stable; pinned HIR nightly **not** needed for this workflow) |
| Lockfile | workspace `Cargo.lock` sha256 `c9c577e982fbc05b969d03853458df0300ca290130bf38364532a59145acb31c` (unchanged); fixture lock generated untimed, resolves `opentelemetry 0.32.0` / `opentelemetry_sdk 0.32.1` offline from the local registry cache |
| Network | `CARGO_NET_OFFLINE=true` for the whole run (offline resolution verified beforehand) |
| Host load | Run 2 (quiet desktop): loadavg 1.85 → 1.83. Run 3 (idle host): loadavg start **0.47**, all desktop applications (browser, chat, IDE, media) closed before launch; end value 3.89 reflects the benchmark's own parallel compilation tail (`cargo -j12` release builds), not other workloads |
| Time window | Run 2: 2026-10-07T10:07:00Z → 10:08:45Z (105 s), exit 0. Run 3: 10:34:45Z → 10:36:27Z (102 s), exit 0 |
| Command | `CARGO_NET_OFFLINE=true cargo bench -p cargo-instrument --bench bench_overhead` |
| Raw evidence | `evidence/phase3-overhead-2026-10-07/` (see section 7) |

## 4. Results

Two successful runs with identical configuration: run 2 on a quiet desktop, run 3 on an idle host (desktop applications closed first). Run 3 is the primary result; medians feed the tables and min/max are the **within-run** spread.

### Run 2 — quiet desktop samples

All individual samples:

| # | clean base (s) | clean inst (s) | repeat base (s) | repeat inst (s) | incr base (s) | incr inst (s) |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | 5.743 | 6.030 | 0.028 | 0.166 | 0.131 | 0.266 |
| 1 | 5.722 | 6.003 | 0.028 | 0.166 | 0.129 | 0.266 |
| 2 | 5.750 | 6.043 | 0.028 | 0.168 | 0.129 | 0.268 |
| 3 | 5.718 | 6.022 | 0.029 | 0.165 | 0.131 | 0.267 |
| 4 | 5.690 | 6.071 | 0.028 | 0.166 | 0.129 | 0.271 |
| **median** | **5.722** | **6.030** | **0.028** | **0.166** | **0.129** | **0.267** |

### Run 2 compile time (N=5, public-command wall time incl. JSON pre-pass, orchestration, mirroring, rewriting)

| Build type | Baseline | Instrumented | Median delta |
| --- | --- | --- | --- |
| Clean | 5.722 s (5.690–5.750) | 6.030 s (6.003–6.071) | **+0.308 s (+5.4 %)** |
| Repeat (no-op) | 0.028 s (0.028–0.029) | 0.166 s (0.165–0.168) | **+0.138 s (+486.6 %)** |
| Incremental (app edit) | 0.129 s (0.129–0.131) | 0.267 s (0.266–0.271) | **+0.138 s (+106.4 %)** |

Both arms compile the identical telemetry-bearing manifest, so this delta isolates instrumentation/orchestration cost — it is **not** comparable to the README's 53.67 % figure (different, larger fixture with a different baseline composition). The repeat and incremental medians show the same absolute +0.138 s in run 2 (+0.150 s in run 3): a consistent per-instrumented-invocation overhead dominates once Cargo has nothing to rebuild. Its composition (pre-pass, session planning, mirror checks, wrapper launch) was **not separately measured** — only the total is observed; the percentage on a 28 ms baseline is misleadingly large, so trust the absolute.

### Run 2 runtime (release, 500,000 calls per sample, counting processor, no exporter)

| Metric | Baseline | Instrumented | Delta |
| --- | --- | --- | --- |
| ns/call (median) | 1.13 ns (1.12–1.45) | 242.64 ns (238.24–251.04) | **+241.51 ns/call** |
| Wall per sample | 0.56–0.72 ms | 119.12–125.52 ms | — |
| Throughput (median) | ~885 M calls/s | ~4.1 M calls/s | — |

Per-sample ns/call — baseline: 1.45, 1.12, 1.12, 1.38, 1.13; instrumented: 245.02, 238.24, 251.04, 239.24, 242.64.

This is the marginal cost of generating, entering/exiting, and **counting** 500k synchronous spans over a telemetry-setup-matched zero-span baseline. It includes the counting processor's per-span work (name prefix check + atomic increment) inside the timed loop; it excludes provider setup, `otel_shim::init()`, export, flush, and shutdown. The baseline arm sits near the measurement floor (~0.6 ms total), so percentages against it are meaningless — only the absolute delta is reported.

### Run 2 binary size (release profile, both arms built in this run)

| Binary | Bytes | Delta |
| --- | --- | --- |
| Baseline | 778,856 | — |
| Instrumented | 834,416 | **+55,560 (+7.1 %)** |

Both binaries contain the telemetry dependencies and SDK setup; the delta is instrumentation-driven code growth for the 20-function dependency (native R-4 route). Stripping/debug/LTO settings identical (Cargo defaults within the pair).

### Run 3 — idle host (primary)

Loadavg 0.47 at launch; browser/chat/IDE/media processes closed beforehand. Emitter route, span-count, and no-`bench_app` assertions all passed again.

| # | clean base (s) | clean inst (s) | repeat base (s) | repeat inst (s) | incr base (s) | incr inst (s) |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | 6.503 | 6.881 | 0.031 | 0.184 | 0.142 | 0.287 |
| 1 | 6.464 | 6.724 | 0.031 | 0.181 | 0.137 | 0.291 |
| 2 | 6.431 | 6.710 | 0.032 | 0.181 | 0.138 | 0.288 |
| 3 | 6.485 | 6.741 | 0.030 | 0.181 | 0.136 | 0.291 |
| 4 | 6.498 | 6.814 | 0.030 | 0.187 | 0.138 | 0.287 |
| **median** | **6.485** | **6.741** | **0.031** | **0.181** | **0.138** | **0.288** |

| Build type | Baseline | Instrumented | Median delta |
| --- | --- | --- | --- |
| Clean | 6.485 s (6.431–6.503) | 6.741 s (6.710–6.881) | **+0.256 s (+4.0 %)** |
| Repeat (no-op) | 0.031 s (0.030–0.032) | 0.181 s (0.181–0.187) | **+0.150 s (+494.6 %)** |
| Incremental (app edit) | 0.138 s (0.136–0.142) | 0.288 s (0.287–0.291) | **+0.150 s (+108.6 %)** |

Runtime per-sample ns/call — baseline: 1.25, 1.28, 1.38, 1.59, 1.14 (median **1.28**, 1.14–1.59); instrumented: 251.62, 250.62, 249.27, 248.60, 251.11 (median **250.62**, 248.60–251.62); median delta **+249.34 ns/call**. Wall per sample: baseline 0.57–0.79 ms, instrumented 124.30–125.81 ms. Throughput: ~781 M vs ~4.0 M calls/s.

Binary size (run 3): 778,856 → 834,416 bytes, **+55,560 (+7.1 %)** — identical binary sizes to run 2 (the logs record file lengths only; no hash or byte comparison was taken).

### Cross-run comparison (medians)

| Metric | Run 2 (quiet desktop) | Run 3 (idle host) | Agreement |
| --- | --- | --- | --- |
| Clean baseline | 5.722 s | 6.485 s | absolute sessions differ ~13 % |
| Clean instrumented | 6.030 s | 6.741 s | ~12 % |
| Clean delta | +0.308 s (+5.4 %) | +0.256 s (+4.0 %) | same order; delta varies ~17 % |
| Repeat delta | +0.138 s | +0.150 s | consistent observed overhead 0.14–0.15 s (composition not measured) |
| Incremental delta | +0.138 s | +0.150 s | same observed overhead |
| Runtime instrumented | 242.64 ns/call | 250.62 ns/call | ~3 % |
| Runtime delta | +241.51 ns/call | +249.34 ns/call | ~3 % |
| Release binary sizes | 778,856 / 834,416 B | identical sizes | sizes reproduce exactly (no hashes recorded) |

**Key pilot finding:** for the **compile** measurements, within-run spread of the instrumented samples reached ~2.5 % of the median (run 3 clean: 6.710–6.881 s), smaller than the ~13 % run-to-run variation of absolute compile medians — so single-process N=5 compile min/max does not capture session-to-session variation. **Runtime** variability is separate and larger within runs: instrumented runtime spread reached ~5.3 % (run 2: 238.24–251.04 ns), and the baseline runtime arm varied up to ~30 % around its ~1.2 ns near-floor median (cross-run runtime medians, by contrast, agreed within ~3 %). Possible explanations for the cross-run differences in absolute compile medians include frequency, thermal, and page-cache state, but none were measured or isolated, and two sessions cannot establish which factors (if any) dominate. Deltas reproduced to ~17 % (clean) and ~3 % (runtime); both runs observed a consistent ~+0.14–0.15 s per-instrumented-invocation overhead whose internal composition was not measured; release binary **sizes** were identical across runs (file lengths only — no hashes recorded).

## 5. Correctness and validation evidence (both successful runs)

- Release route: bench asserted and printed `EMITTER: ... crate=bench_dep] selecting native R-4 emitter`, `transformed 20 candidates into mirror ...`; asserted **no** `crate=bench_app` wrapper lines (dependencies-v1 excludes the workspace app).
- Every instrumented runtime sample asserted exactly 500,000 completed `compute_step_*` spans; every baseline sample asserted 0. Each app asserts its output accumulator ≠ 0.
- Workflow correctness re-verified on Linux this session: `phase3_coverage_baseline` static census + trace oracle (pre-fix and post-fix), whole `dependency_instrumentation_e2e_tests` file (6/6) after the fix.
- The bench does **not** verify parent-child ancestry of counted spans (covered by the fixture trace oracle, not this benchmark), and the debug builds' route is asserted only for the release build (existing documented limitation).

## 6. Limitations (pilot gaps vs the evaluation protocol)

- **Pilot, not a completed protocol run**: N=5 build and N=5 runtime samples per run (protocol asks ≥10 runtime); median/min/max only; no warm-up run discarded, no order alternation (baseline always timed first within a pair — OS file-cache warmth slightly favors the instrumented arm), no confidence analysis. The two-run comparison (section 4) shows within-run min/max does not capture run-to-run variation (for compile; runtime cross-run medians agreed more closely than their within-run spread).
- Runtime arm set is only "telemetry setup, zero spans" vs "generated spans + counting" (protocol variants 2 vs 3). There is **no plain app without telemetry dependencies** arm, so per-call deltas must not be read as instrumented-vs-uninstrumented-app.
- Synchronous fixture only: nothing here measures async workloads, exporter/collector cost, sampling-off/no-op modes, first-party HIR runtime cost, or C-ABI fallback cost. The native R-4 route was measured; the fallback was observed only as the run-1 failure.
- Counting/verification cost is inside the timed region (disclosed above). Two runs, one host: run 2 with a quiet desktop resident, run 3 idle — absolute compile medians differed by up to ~13 % between sessions; the causes were not measured or isolated (section 4). Within-run min/max (tabled per run) is the only spread this harness reports; it does not bound session-to-session variation.
- Raw logs are preserved at `evidence/phase3-overhead-2026-10-07/` (section 7); `/tmp` working copies may be cleaned on reboot.
- The section-2 fix was **not committed** at the time of this note (working-tree change awaiting maintainer review).

## 7. Evidence location and how to rerun

Raw evidence location: `evidence/phase3-overhead-2026-10-07/` (working copies also under `/tmp/opencode/phase3-bench-2026-10-07/`, which is not durable across reboots). Log sha256: run 1 `7675d109…`, run 2 `88062228…`, run 3 `063a4df4…` (full hashes recorded alongside the files).

| File | Content |
| --- | --- |
| `run3_idle_host.log` / `run2_quiet_desktop.log` | Successful runs 3 and 2: complete stdout/stderr, start/end loadavg, `BENCH_EXIT=0` |
| `run1_FAILED_tier2_fallback.log` | Run 1 complete output through `BENCH_EXIT=101` (all compile samples + panic) |
| `run1_failure_repro_tier2_release.stderr` / `fix_verification_r4_release.stderr` | Standalone release repro before/after the fix (full Tier-2 reason, then native R-4) |
| `fix_r4_release_after_review_correction.stderr` / `fix_r4_release_after_cfg_correction.stderr` | Release repro re-runs after the two review corrections (native R-4, exit 0) |
| `env-manifest.txt` / `env-manifest-run2.txt` | Environment/version manifests (revision, toolchain, lockfile hash unchanged across runs) |
| `run-bench.sh` | Exact runner |

```sh
# Correctness for the measured workflow (~10 s warm):
cargo test -p cargo-instrument --test dependency_instrumentation_e2e_tests

# Full benchmark (the measured run):
CARGO_NET_OFFLINE=true cargo bench -p cargo-instrument --bench bench_overhead

# Regression suite for the section-2 fix:
cargo test -p cargo-instrument --lib
cargo test -p cargo-instrument --test native_artifact_compatibility_tests
cargo test -p cargo-instrument --test tokio_spawn_tests
```

## 8. Next bounded experiment supported by these findings

1. Raise the sample count and independence: N=10 runtime samples (protocol minimum), one discarded warm-up, alternating pair order, and repetitions across separate sessions — section 4 shows between-session compile-medians variation (~13 %) exceeds the within-run compile spread (~2.5 %), so session-to-session replication is needed for compile results in addition to a larger intra-run runtime sample count. Samples are cheap (≈2 min per run).
2. Add the missing plain-app runtime arm (telemetry deps removed) as a third variant so setup cost separates from per-span cost — requires a harness change, so it is a deliberate next step rather than part of this pilot.
3. The observed +0.14–0.15 s per-instrumented-invocation overhead (consistent across both runs and both repeat/incremental scenarios) dominates the no-op/incremental delta. Its composition was not measured; instrumenting the substeps is the obvious follow-up before treating it as a fixed, attributable cost.

Phase 3 remains open: this note covers a deterministic synchronous fixture only (roadmap sections 3.2/3.3/3.4 pilot evidence), not corpus, async, fallback, export, or comparative results.
