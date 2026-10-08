# Phase 3 — Evaluation & Research

[Project README](../../README.md)

Status: proposed execution roadmap, 2026-10-05. Phase 2 evidence is a starting point, not a Phase 3 result.

First evidence under this roadmap: the benchmark audit, small deterministic coverage fixture, and trace oracle recorded in the [coverage baseline note](phase-3-coverage-baseline.md) (executed 2026-10-05). It is bounded fixture evidence only; no section below is marked complete and no performance result is claimed. The initial overhead benchmark timing result is recorded in the [historical overhead pilot note](phase-3-overhead-pilot-2026-10-07.md) (executed 2026-10-07; also documenting a release-profile route-selection defect found and fixed). The strengthened three-session synchronous evaluation (alternating pair order, N=10 runtime samples, and separate-process warm-up) is recorded in the [three-session overhead evaluation note](phase-3-overhead-pilot-2026-10-08.md) (executed 2026-10-08). The in-process steady-state warm-up baseline across three sessions is recorded in the [steady-state overhead evaluation note](phase-3-overhead-pilot-2026-10-08-steady-state.md) (executed 2026-10-08). The repeat-build overhead profiling and stage attribution is recorded in the [repeat-build profiling note](phase-3-repeat-build-profiling-2026-10-09.md) (executed 2026-10-09).

The question for this phase is whether the implemented instrumentation provides useful coverage and correct context at an acceptable cost in real Rust applications. Evaluate the existing first-party HIR application workflow and dependency wrapper separately and together. Preserve the accepted architecture; evaluation findings can motivate focused fixes or a separate design discussion.

## Starting evidence and gaps

| Existing asset | What it provides | What Phase 3 still needs |
| --- | --- | --- |
| [Overhead benchmark](../../cargo-instrument/benches/bench_overhead.rs) | Five-sample clean, repeat, and app-edit timings; synchronous SDK counting-processor runtime cost; release executable size | Explicit workflow/emitter verification, async workloads, matched manual comparisons, raw durable results |
| [Scale benchmark](../../cargo-instrument/benches/bench_scale.rs) | Synthetic graph planning and a 30-dependency public build fixture | Real graphs, source-density variation, resource measurements |
| [Public scale/incremental tests](../../cargo-instrument/tests/scale_incremental_e2e_tests.rs) | Broad/deep/diamond fixtures, spans, cache cycles, fallback, source preservation, gated first-party apply measurement | Corpus evaluation and controlled timing across the complete user workflow |
| [Native OTel tests](../../cargo-instrument/tests/native_otel_tests.rs), [hybrid tests](../../cargo-instrument/tests/hybrid_instrumentation_tests.rs), [spawn tests](../../cargo-instrument/tests/tokio_spawn_tests.rs) | Existing emission, runtime parenting, coexistence, and conservative spawn recognition cases | A common trace oracle, repeated concurrency trials, separate first-party/dependency evidence |
| [Dependency E2E tests](../../cargo-instrument/tests/dependency_instrumentation_e2e_tests.rs) | Public dependency workflow, artifact identity, failure paths | Function/unit coverage census and long-running/failure recovery experiments |

The README records 53.67% clean-build overhead for the completed scale fixture against its approved 55% fixture budget. Retain this as historical fixture evidence. It supplies neither a universal build budget nor a runtime target. No tests or benchmarks were rerun while preparing this roadmap.

The overhead benchmark currently launches `cargo-instrument -- build` without explicit `--with-dependencies` and inherits instrumentation environment settings. Its intended dependency-span assertions do not establish that it exercises today's public dependency workflow. Audit the selected route and make the experiment configuration explicit before adopting its numbers.

*Audit completed 2026-10-05 (see the [baseline note](phase-3-coverage-baseline.md)): the flagless invocation was observed running policy `legacy-v1` (instrumenting both the workspace application and the excluded path dependency), and ambient `CARGO_INSTRUMENT_DEPENDENCIES` was observed flipping a flagless run to `dependencies-v1`. The benchmark now invokes `--with-dependencies` explicitly, removes ambient instrumentation environment variables, checks every build subprocess, asserts the native R-4 route for the dependency, and prints raw samples. The full suite has not been rerun, so no timing result is claimed.*

## Evaluation protocol

Start with deterministic local fixtures: synchronous nested calls, an async request/task tree, and an unowned dependency graph. Then select a small real-application corpus spanning a synchronous CLI, a Tokio service, and an application with a substantial dependency graph. Record repository URLs, exact revisions, licenses, build instructions, and workload inputs before running. Corpus selection remains open; examples alone do not count as real-application evidence.

For every run, retain the project/tool revision, generated first-party patch, lockfile/hash, `rustc -vV`, Cargo version, OS/architecture, CPU/memory, features, profile, compiler flags, target directory, command, environment overrides, sampler/processor/exporter configuration, worker count, workload seed, repetitions, and exit status. Record the pinned HIR toolchain from [tools/p23-toolchain.txt](../../tools/p23-toolchain.txt) and the exact stable compiler used for generated builds. Use the repository's compatible OTel versions initially; do not upgrade dependencies to match latest documentation.

Keep benchmark source and small summaries reviewable in the repository. Retain raw samples, commands, logs, trace assertions, and failure records with each result; identify their durable location in the summary. Keep generated targets, mirrors, large traces, and temporary fixtures out of commits. Begin with simple result files and existing harnesses; a new benchmarking framework is not required.

Use separate artifact directories for each variant. Resolve/fetch dependencies before timed builds and use locked, offline runs where possible. A clean build means an empty variant artifact directory, not a cold OS filesystem cache. Report driver/tool bootstrap, first-party source application, subsequent normal builds, and dependency orchestration separately. Also report the end-to-end setup cost a user pays.

Run correctness checks outside timed sections. Warm runtime workloads before collecting samples, alternate baseline/variant order, and run on an otherwise idle host. Start with at least five independent build samples and ten independent runtime samples; retain individual values and report medians and spread. Pilot results determine whether more samples are needed. Tail latency needs enough requests and a documented load model, not ten timing values. Report absolute deltas alongside percentages; tiny baselines can make ratios misleading.

No speed result is valid unless workload outputs agree and the intended instrumentation is demonstrated. Separate no-provider/no-op, sampling-off, recording to a bounded counting processor, and batch export experiments. Counting/verification work has a cost; identify where it is included. Exporter/collector availability, queue drops, flush time, and shutdown time belong in export results. Avoid retaining every span in memory during throughput or soak tests.

## Comparison variants

| Variant | Purpose |
| --- | --- |
| Uninstrumented application | Practical baseline without telemetry setup or generated spans |
| Same telemetry dependencies and setup, no application spans | Separate initialization/dependency cost from per-span cost |
| Generated native OTel: first-party only | Evaluate source application and steady-state builds/runtime |
| Generated native OTel: dependencies only, then combined | Evaluate incremental coverage and wrapper/orchestration cost |
| C ABI fallback, where safely selected | Measure synchronous fallback separately; record unsupported async/spawn coverage |
| Manual native OTel | Reference for generated API/lifecycle overhead and context correctness |
| Manual `tracing` | Measure the async-aware span model with a documented subscriber/filter |
| Manual `tracing` → OTel | Measure bridge cost under the same compatible SDK/sampler/processor |
| Tokio runtime telemetry | Compare task/resource visibility and runtime collection cost under its own configuration |

Run two comparison tracks. **Matched coverage** uses the same function boundaries, span names/attributes, execution counts, and relevant lifecycle semantics in controlled fixtures. Use current eligibility to construct a common workload; do not add a production filtering API just for the comparison. **Practical coverage** uses each approach as available in the real application and reports spans/functions/tasks observed alongside total cost. Extra dependency spans must not be presented as unexplained emitter overhead.

Tokio runtime telemetry is a candidate visibility comparison, not an assumed substitute for function-level OTel spans. Select and pin the concrete collector before implementing this arm. For example, `console-subscriber` collects runtime task/resource diagnostics and currently requires Tokio tracing features and `tokio_unstable`; record these flags and compare against an otherwise identical runtime build with collection disabled. It is not currently a repository dependency. Introducing it requires a separate dependency decision.

## 3.1 Coverage Evaluation

Build a census with stable site identities: package/version, Cargo unit, source location, function form, frontend, eligibility reason, selected emitter, and generated/observed status. Inspect source and generated edits independently of the candidate count so omissions by discovery are visible.

Report total in-scope functions, eligible functions, instrumented functions, intentionally excluded functions, and unexpected misses. Break down free functions, methods, generics, sync/async, Result shapes, macro-owned/generated code, and existing manual instrumentation. Keep HIR and syntactic wrapper denominators separate. A function definition is not a monomorphization, and a package is not a Cargo unit.

Report `instrumented / eligible` and `instrumented / total in-scope` separately. Dynamic coverage is verified expected invocations/spans for an exercised workload, not all static functions. Separate a missing edit, an unexecuted function, a sampled-out span, and an exporter drop. Check that explicit instrumentation is preserved without duplicate automatic spans.

Deliverable: coverage tables for local fixtures and the selected corpus, exclusion counts, and minimal reproductions for unexpected gaps. Complete when every census site has a disposition and exercised sites agree with the independent oracle.

## 3.2 Compilation Overhead

Measure tool/driver preparation and first-party apply, then clean debug/release builds, repeat builds, app-only edits, dependency-only edits, and the combined workflow. Include dependency native artifact acquisition/pre-pass time in public-command totals; measure substeps only when existing logs or external timing can do so reliably.

Use telemetry-matched and practical baselines. Record wall time, available CPU time/peak memory measurements, source/mirror bytes, and rebuilt Cargo units. Keep instrumentation frontend/emitter selection in the result. Unsupported command/target configurations are separate compatibility cases, not successful native measurements.

Deliverable: paired timing/resource tables with raw samples and workload sizes. Complete when setup and steady-state costs are distinguishable and results reproduce within the reported spread.

## 3.3 Runtime Overhead

Extend beyond tiny synchronous calls to nested call chains, CPU work, yielding futures, fan-out/fan-in, and an application request workload. Measure added ns/call or ns/span where meaningful, throughput, request p50/p95/p99, CPU, memory, and span volume. Keep release optimization, inputs, worker counts, and offered load identical within each pair.

Measure generated native and fallback costs separately. Use matched manual OTel, `tracing`, and bridge runs to identify API, subscriber, and bridge costs. Sampling-off and no-op results must accompany recording results; none alone describes production overhead.

Deliverable: microbenchmark and application tables by telemetry mode and density. Complete when output/span validation passes and measurement uncertainty is reported. Performance acceptance budgets remain open until pilots establish realistic use cases.

## 3.4 Binary Size

Compare release executable bytes and, where platform tools permit, code/read-only-data/debug section sizes. Hold stripping, debug info, LTO, codegen units, target, and panic strategy constant within a pair. Report telemetry dependency/setup growth separately from instrumentation growth.

Measure first-party, native dependency, fallback, manual, and combined variants at multiple function densities. Total target-directory size belongs in cache/storage results, not executable size.

Deliverable: absolute bytes and percentage growth per configuration, with commands and artifact identity. Complete when sizes refer to binaries that built and passed the relevant output/span checks.

## 3.5 Cache / Incremental Compilation

Exercise plain → instrumented → plain builds; instrumented no-change repeats; app/dependency edits; feature/profile changes; native/fallback policy changes; and a tool/emission revision change. Verify spans and application results after each transition. Check selected rebuilt units, stale mirror/artifact reuse, invalidation scope, target/mirror storage, and source/manifest/lockfile preservation.

Evaluate first-party apply idempotence on reviewed, committed generated source using fixture-local Git repositories. Do not edit or commit a corpus contributor's working tree implicitly. Separate Cargo incremental compilation from wrapper source-mirror/session reuse; a fast command does not prove either is correct.

Deliverable: transition matrix with timings, rebuild evidence, and span checks. Complete when no transition silently reuses incompatible instrumentation. Any revision-invalidation gap becomes a focused defect with a reproduction.

## 3.6 Async Correctness

Create an expected trace graph per deterministic workload. Compare trace IDs, parent span IDs, names, execution counts, and applicable status/outcome attributes. Validate first-party generated code, native dependencies, and hybrid manual/generated trees separately; do not assume they have identical lifecycle metadata.

Cover first poll, repeated Pending, Ready, unpolled drop, cancellation after first poll, timeout/abort, Err/early return, unwind, nested awaits, and supported nested spawns. Force sibling-task interleaving with barriers/manual polling. Force cross-thread polling and record the threads actually used; merely enabling Tokio's multithreaded scheduler does not prove migration occurred.

Assert one span per eligible invocation, correct ancestry, no context leakage while suspended, restored caller context after polling/drop/unwind, and exactly-once end where observable. Native dependency lifecycle expects completed/cancelled/unwound according to the implemented contract; first-party spans do not currently carry that dependency outcome guard. Panic-abort runs cannot require drop-based cleanup. Unsupported aliases, `spawn_blocking`, local spawns, other runtimes, and Stream/Sink polling belong in the boundary matrix.

Deliverable: executable trace oracle and repeated deterministic/concurrent runs with failure traces. Complete when supported cases have zero observed oracle violations and unsupported cases are explicitly classified. State trial counts; this is bounded evidence, not proof for every schedule.

## 3.7 Dependency Coverage

Evaluate direct/transitive path and registry dependencies, multiple versions, renamed bindings, shared diamonds, target/feature variants, proc-macro/build-script host units, and telemetry/executor exclusions. Verify the public `--with-dependencies` path and actual selected native/fallback routes.

Report package, Cargo-unit, function, and exercised invocation coverage separately. Audit source discovery across modules and active cfg/features. Include incompatible/ambiguous artifacts, absent shim provider, unsafe restrictions, and unsupported async sites; distinguish deliberate safe skips from missed eligible sites. Verify original dependency files/manifests and lockfiles remain unchanged after preparation.

Deliverable: dependency census, cross-crate trace graphs, and reasons for every skipped unit/site. Complete when reported coverage is backed by edits/artifacts and runtime evidence rather than wrapper invocation alone.

## 3.8 Comparative Evaluation

Run the comparison variants after the coverage and async oracles are stable. Use the same workload and compatible telemetry stack for matched arms, and publish practical coverage as its own table. Preserve existing manual instrumentation and report hybrid parent continuity.

Compare compilation/setup cost, runtime cost, executable size, observed function/dependency coverage, context correctness, and manual source changes required. For Tokio telemetry, report supported task/resource observations; mark unavailable function-span/context metrics as not comparable. Do not invent missing spans to force identical semantics.

Deliverable: a comparison report explaining which approach suits which workload and the limits of the comparison. Complete when each conclusion links to reproducible measurements; a performance ranking alone is insufficient.

## 3.9 Stress & Adversarial Evaluation

Increase load in bounded steps after ordinary correctness passes. Record limits, timeout, peak resources, last successful level, first failing level, and recovery behavior. Begin from existing scale fixtures and use pilot resource usage to choose larger levels. Avoid an unbounded default stress command.

| Dimension | Experiment | Evidence required |
| --- | --- | --- |
| Compilation stress | Larger modules/generic instantiations, debug/release, varied Cargo jobs and link settings | Build/output correctness, wall time, peak memory, controlled timeout |
| Dependency-graph stress | Wider/deeper/diamond graphs, duplicate versions and feature variants | Unit/mirror isolation, selected routes, graph coverage, resource scaling |
| Async concurrency stress | Many unrelated roots, nested spawn trees, forced yielding/migration and cancellation | No cross-trace ancestry or leaked context; counts/outcomes reconcile |
| Instrumentation-density stress | More eligible functions and deeper/hotter call chains, holding work fixed | Cost versus spans/call, binary growth, compiler resource use |
| Pathological Rust | Lifetimes, borrowed outputs, trait/boxed futures, recursion, macros/cfg, unsafe restrictions, Unicode/CRLF | Same program behavior/diagnostics or documented safe exclusion |
| Failure injection | Existing malformed artifact/pre-pass hooks, interrupted mirror writes, missing artifacts, exporter failure/backpressure | Correct failure propagation or documented safe fallback; coverage loss recorded; subsequent clean recovery |
| Concurrent invocations | Separate target directories, then shared directories and competing instrumentation policies | No corrupt plans/mirrors/artifacts; observed serialization/rejection/recovery; correct resulting spans |
| Long-running runtime stress | Bounded soak with task churn, cancellation and slow/failing export, followed by drain/shutdown | Memory trend after warmup, active/completed/dropped counts, bounded queues, shutdown/flush behavior |

Deliverable: stress envelope and defect reproductions, including successful recovery checks. Complete when all eight dimensions have bounded evidence or a concrete documented blocker. A collector outage must not be reported as zero-cost successful tracing.

## Execution order and first work items

The numbering groups research questions; it is not a requirement to run them sequentially.

1. **Protocol and fixture audit:** record environment/version manifests; audit existing benchmarks for public CLI flags, selected emitter, subprocess failures, raw sample retention, and span verification. Draft the fixture census and expected trace graphs. Reuse existing exporters/processors and fixtures where possible. *Benchmark audit and a first fixture census with an expected trace graph are recorded in the [baseline note](phase-3-coverage-baseline.md), including the environment/version manifest for that run.*
2. **Correctness baseline (3.1, 3.6, 3.7):** establish supported/excluded boundaries and baseline trace correctness for local fixtures. Capture existing behavior before any defect fix; keep fixes separate from measurement changes.
3. **Cost pilot (3.2–3.5):** run controlled local measurements, identify noisy or invalid experiments, and propose workload-specific budgets before larger runs. Preserve the old scale budget only for its original fixture.
4. **Corpus and comparisons (3.8 plus 3.1–3.7):** select/pin real applications and the concrete Tokio telemetry arm, then repeat correctness and cost measurements. Review any new dependency or materially different implementation strategy before adding it.
5. **Stress and synthesis (3.9):** run bounded stress/soak experiments and publish findings, known failures, coverage limits, reproduction instructions, and recommended follow-up work.

The first implementation task is the existing benchmark audit and a small deterministic coverage/async oracle. Selecting corpus repositories, accepting performance budgets, adding a runtime telemetry dependency, and changing instrumentation architecture remain explicit decisions. This roadmap does not authorize those architecture changes or reopen the closed eBPF implementation branch in ADR-005.

Phase 3 is complete when all nine sections have reproducible evidence or explicit blockers, comparisons disclose coverage/configuration differences, correctness defects are resolved or bound the stated support, and the report makes clear which application classes were actually evaluated. Completion does not itself establish production readiness.

## Research references

- [OpenTelemetry 0.32 `FutureExt`](https://docs.rs/opentelemetry/0.32.0/opentelemetry/trace/trait.FutureExt.html): context is current while a wrapped future is polled.
- [`tracing` async span guidance](https://docs.rs/tracing/latest/tracing/struct.Span.html#in-asynchronous-code): `Instrument` enters/exits around polling; holding an enter guard across await gives incorrect traces.
- [`tracing-opentelemetry` 0.33](https://docs.rs/tracing-opentelemetry/0.33.0/tracing_opentelemetry/): bridge from tracing spans into OTel tracing.
- [`console-subscriber` configuration](https://docs.rs/console-subscriber/latest/console_subscriber/): runtime diagnostics, required Tokio tracing configuration, and collection/export setup.

References were consulted on 2026-10-05. Pin the resolved versions and feature flags in each experiment; latest documentation is not evidence of repository compatibility.
