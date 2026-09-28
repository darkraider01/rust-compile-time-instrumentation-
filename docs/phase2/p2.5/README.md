# P2.5 implementation and acceptance plan

**Status:** In progress. Current evidence and outstanding work are in [closeout.md](closeout.md).

P2.5 covers graph scale, tracer acquisition, performance budgets, and platform/toolchain validation. P2.4 remains the accepted behavior baseline.

| Criterion | Evidence required | Current status |
| --- | --- | --- |
| Planning across broad, deep, and layered graphs | Synthetic metadata at 10, 50, 100, and 200 library packages; planning assertions and timings | Implemented |
| Live instrumentation at scale | Public dependency workflow, eligible unowned dependencies, transformation proof; broader live topology/size coverage | Partial: benchmark has 30 broad path dependencies; legacy wrapper fixtures cover 105 layered, 15 deep, and 20 broad libraries |
| Tracer caching decision | Explicit no-op and SDK provider measurements; provider replacement and concurrent scope correctness | Implemented; static caching rejected because provider replacement leaves cached handles stale |
| Reproducible performance evaluation | Raw samples, medians, cache conditions, environment, actual budget checks | Corrected harness; limits remain provisional pending representative baselines |
| Platform/toolchain certification | Successful remote jobs with run URLs and exact toolchains | Pending; existing matrices are configured, not certified |

Run `cargo bench -p cargo-instrument --bench bench_scale --offline` after fetching repository dependencies. The benchmark reports in-memory planning for 12–202 packages and five public dependency build pairs. Metadata package counts are not compilation-unit counts: target, profile, feature, and host combinations can produce additional units.

Follow-up work includes larger live public graphs across all topologies, first-party apply scale measurements, incremental dependency/app edit measurements, and remote platform/toolchain runs. Keep P2.5 open until those coverage boundaries are accepted or completed.
