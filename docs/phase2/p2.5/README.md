# P2.5 implementation and acceptance plan

**Status:** In progress (clean build overhead reduced from 117.3% to 48.5%, scale and incremental test suites implemented; platform runs for the current revision pending; clean-build budget acceptance pending review of proposed 55% threshold). Current evidence and detailed logs are in [closeout.md](closeout.md).

P2.5 covers graph scale, tracer acquisition, performance budgets, and platform/toolchain validation. P2.4 remains the accepted behavior baseline.

| Criterion | Evidence required | Current status |
| --- | --- | --- |
| Planning across broad, deep, and layered graphs | Synthetic metadata at 10, 50, 100, and 200 library packages; planning assertions and timings | Implemented (`benches/bench_scale.rs`, `tests/scale_incremental_e2e_tests.rs`) |
| Live instrumentation at scale | Public dependency workflow and transformation proof across broad (100 deps), layered diamond (100 deps), and deep (20 deps) topologies | Implemented locally; current-revision CI pending |
| Incremental workflow coverage | Clean, repeat/no-op, app-only edits, dep-only edits, clean rebuilds, parallel build isolation, and input immutability | Passed locally (5 tests) on current revision; remote matrix pending |
| First-party lint-apply scale | Scale measurements on 5 crates (100 functions) using pinned driver toolchain | Passed locally on pinned nightly (`tests/scale_incremental_e2e_tests.rs`, 14.06s, 2.81s/crate); remote CI pending |
| Tracer caching decision | Explicit no-op and SDK provider measurements; provider replacement and concurrent scope correctness | Implemented; static caching rejected due to provider replacement hazard |
| Reproducible performance evaluation | Raw samples, medians, cache conditions, environment, actual budget checks | Implemented (`bench_scale`: planning 21.6 µs/pkg PASS, repeat 0.613s PASS, clean overhead 48.5% vs 40% provisional limit FAIL) |
| Platform/toolchain certification | Remote jobs with run URLs, target triples, and resolved toolchain versions | Baseline commit b771da5 verified (CI run 36386894186, Integration run 36386894301, P2.3 Apply run 36386894178); current-revision verification pending |

Run `cargo bench -p cargo-instrument --bench bench_scale --offline` after fetching repository dependencies. The benchmark reports in-memory planning for 12–202 packages and five public dependency build pairs. Metadata package counts are not compilation-unit counts: target, profile, feature, and host combinations can produce additional units.

Run `cargo test -p cargo-instrument --test scale_incremental_e2e_tests` to execute the complete live graph scale and incremental cycles test suite.
