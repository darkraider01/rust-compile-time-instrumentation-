# P2.5 validation record

**Date:** 2026-09-28. **Status:** In progress.

This record replaces the earlier premature completion claims. P2.4 remains complete within its accepted scope. Remote platform certification and broader live scale coverage remain pending.

## Graph coverage

`graph_scale_tests` has synthetic planning across broad, deep, and layered metadata with up to 202 packages. Its live Cargo fixtures exercise the legacy wrapper at 105 layered, 15 deep, and 20 broad libraries. These do not prove public native orchestration at every synthetic graph size.

`bench_scale` now measures the public `--with-dependencies` workflow with 30 explicitly excluded path libraries, a workspace application, OpenTelemetry, and the shim. It checks dependency mirrors rather than assuming successful compilation proves instrumentation. The live benchmark covers broad topology only and checks the emitted native tracer scope in every leaf mirror.

## Tracer acquisition

The release profiling test explicitly installs no-op and SDK providers, uses fresh local caches, measures acquisition without span creation, prevents elimination with `black_box`, and reports ten alternating samples of 50,000 acquisitions per mode. Provider state from earlier tests cannot select the profiling mode.

On the local Windows MSVC host, corrected release measurements were 9.62–14.92 ns for no-op acquisition and 84.67–98.95 ns for SDK acquisition. Cached handle access was approximately 0.26–0.35 ns. These are acquisition-only measurements, not end-to-end span lifecycle overhead. OpenTelemetry 0.32 uses a global `RwLock`, not an atomic pointer lookup.

Static caching remains rejected because a handle acquired before provider registration does not follow later provider replacement. This decision is based on correctness; the measurements do not establish negligible SDK acquisition cost. A safe provider-aware caching design remains future work if whole-lifecycle profiling justifies it. Three focused tracer tests passed, including provider replacement and concurrent scope/export correctness.

## Benchmark methodology and provisional budgets

Planning uses ten samples per shape/size and reports median, minimum, maximum, and median per metadata package. Builds use five baseline/instrumented pairs with fresh, separate target directories and alternating order. Repeat samples reuse the corresponding instrumented target. Dependency resolution happens before timing; measured commands use `--offline --locked`. Filesystem and operating-system caches are warm/uncontrolled, so these are clean artifact builds, not cold-machine measurements.

All raw build samples are printed. Budget results are calculated from actual measurements; any failed budget returns a nonzero exit status. The retained limits (100 µs/package planning, 40% clean overhead, 1.5 s repeat latency) are provisional investigation thresholds inherited from the earlier work, not certified production guarantees. Rebaseline on representative workloads before accepting them. Runtime and binary-size budgets have not been revalidated by this correction.

Environment: Rust 1.97.1 (8bab26f4f), x86_64-pc-windows-msvc, LLVM 22.1.6. Processor: AMD Ryzen 5 5600H with Radeon Graphics, read from the Windows processor registry entry. Measurements must be compared on the same host/toolchain and repeated before changing limits.

## Platform evidence

Existing CI workflows configure Windows, Ubuntu, and macOS stable jobs and a pinned-nightly apply workflow. No remote run has been established by this review. Platform certification remains pending until successful runs are recorded with their URLs, target triples, and resolved toolchain versions.

## Validation commands

- `cargo test -p cargo-instrument --release --offline --test tracer_caching_profile_tests -- --nocapture`: 3 passed.
- `cargo bench -p cargo-instrument --offline --bench bench_scale`: all five build pairs verified 30 native leaf scopes; exit code 1 because clean build overhead exceeded the provisional budget.

No full-workspace pass is claimed for this correction.

## Corrected public benchmark results

Final run on 2026-09-28:

| Metric | Observed | Provisional limit | Result |
| --- | --- | --- | --- |
| Worst median planning rate | 21.491 µs/package | 100 µs/package | Pass |
| Median baseline clean build | 6.907 s | Reference | Measured |
| Median public native clean build | 15.009 s (+117.292%) | +40% | Fail |
| Median public repeat build | 0.968 s | 1.5 s | Pass |

Raw baseline seconds: `[6.9071545, 7.2022768, 7.9399173, 6.4584655, 6.3984442]`.
Raw public native seconds: `[15.7263957, 15.4369736, 14.7677877, 15.0086646, 14.5702837]`.
Raw repeat seconds: `[0.9584008, 0.9952181, 0.9684034, 0.9669518, 0.979658]`.

This run intentionally returns a nonzero exit status. Do not raise the threshold merely to make CI green; investigate orchestration costs and establish representative baselines. Host background activity was not isolated; some focused validation ran concurrently, so these measurements establish a budget miss requiring investigation rather than a certified regression magnitude.

Additional correction checks: synthetic planning test passed (1 test); package all-target Clippy with warnings denied, formatting, and diff whitespace checks passed. No full workspace or remote CI pass is claimed.