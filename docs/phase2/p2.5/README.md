# P2.5 implementation and acceptance plan

**Status:** Complete (latest clean build overhead is 53.67%, within the approved 55% budget; remote platform and pinned nightly certification completed on commit `4a19848`). Current evidence and detailed logs are in [closeout.md](closeout.md).

P2.5 covers graph scale, tracer acquisition, performance budgets, and platform/toolchain validation. P2.4 remains the accepted behavior baseline.

| Criterion | Evidence required | Current status |
| --- | --- | --- |
| Planning across broad, deep, and layered graphs | Synthetic metadata at 10, 50, 100, and 200 library packages; planning assertions and timings | Implemented & verified (`benches/bench_scale.rs`: latest max 22.77 µs/pkg, PASS vs 100 µs/pkg limit) |
| Live instrumentation at scale | Public dependency workflow and transformation proof across broad (100 deps), layered diamond (100 deps), and deep (20 deps) topologies | Certified on Windows, Linux, and macOS (Integration run 36419389229, CI run 36419389415) |
| Incremental workflow coverage | Clean, repeat/no-op, app-only edits, dep-only edits, clean rebuilds, parallel build isolation, and input immutability | Certified across Windows, Linux, and macOS matrix (5/5 tests passed on all platforms) |
| First-party lint-apply scale | Scale measurements on 5 crates (100 functions) using pinned driver toolchain | Certified on remote `ubuntu-latest` with pinned `nightly-2026-09-09` (run 36419389351: 7.70s total, 1.54s/crate) |
| Tracer caching decision | Explicit no-op and SDK provider measurements; provider replacement and concurrent scope correctness | Implemented; static caching rejected due to provider replacement hazard |
| Reproducible performance evaluation | Raw samples, medians, cache conditions, environment, actual budget checks | PASS (`bench_scale`: planning 22.77 µs/pkg, repeat 0.928s, clean overhead 53.67% vs approved 55% limit) |
| Platform/toolchain certification | Remote jobs with run URLs, target triples, and resolved toolchain versions | Certified on commit `4a19848` (CI run 36419389415, Integration run 36419389229, P2.3 Apply run 36419389351) |

Run `cargo bench -p cargo-instrument --bench bench_scale --offline` after fetching repository dependencies. The benchmark reports in-memory planning for 12–202 packages and five public dependency build pairs. Metadata package counts are not compilation-unit counts: target, profile, feature, and host combinations can produce additional units.

Run `cargo test -p cargo-instrument --test scale_incremental_e2e_tests` to execute the complete live graph scale and incremental cycles test suite.
