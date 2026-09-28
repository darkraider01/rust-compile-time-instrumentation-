# P2.5 implementation and acceptance plan

**Status:** In progress (clean build overhead measured at 48.51%, scale and incremental test suites verified locally; platform certification of test fixture fix pending remote run; clean-build budget acceptance pending review of proposed 55% threshold). Current evidence and detailed logs are in [closeout.md](closeout.md).

P2.5 covers graph scale, tracer acquisition, performance budgets, and platform/toolchain validation. P2.4 remains the accepted behavior baseline.

| Criterion | Evidence required | Current status |
| --- | --- | --- |
| Planning across broad, deep, and layered graphs | Synthetic metadata at 10, 50, 100, and 200 library packages; planning assertions and timings | Implemented & verified (`benches/bench_scale.rs`: 13.06–31.25 µs/pkg, PASS vs 100 µs/pkg limit) |
| Live instrumentation at scale | Public dependency workflow and transformation proof across broad (100 deps), layered diamond (100 deps), and deep (20 deps) topologies | Passed locally (`tests/scale_incremental_e2e_tests.rs`); remote certification of test fixture fix pending push |
| Incremental workflow coverage | Clean, repeat/no-op, app-only edits, dep-only edits, clean rebuilds, parallel build isolation, and input immutability | Passed locally (5/5 tests in 98.29s); remote matrix pending push |
| First-party lint-apply scale | Scale measurements on 5 crates (100 functions) using pinned driver toolchain | Passed locally on pinned nightly-2026-09-09 (9.07s total, 1.81s/crate); remote CI pending push |
| Tracer caching decision | Explicit no-op and SDK provider measurements; provider replacement and concurrent scope correctness | Implemented; static caching rejected due to provider replacement hazard |
| Reproducible performance evaluation | Raw samples, medians, cache conditions, environment, actual budget checks | Implemented (`bench_scale`: planning 31.25 µs/pkg PASS, repeat 0.590s PASS, clean overhead 48.51% vs 40% provisional limit FAIL) |
| Platform/toolchain certification | Remote jobs with run URLs, target triples, and resolved toolchain versions | Commit `a27d93d` failed in CI/Integration/Apply due to test runner ANSI/git issues; test fixes validated locally, remote rerun pending push |

Run `cargo bench -p cargo-instrument --bench bench_scale --offline` after fetching repository dependencies. The benchmark reports in-memory planning for 12–202 packages and five public dependency build pairs. Metadata package counts are not compilation-unit counts: target, profile, feature, and host combinations can produce additional units.

Run `cargo test -p cargo-instrument --test scale_incremental_e2e_tests` to execute the complete live graph scale and incremental cycles test suite.
