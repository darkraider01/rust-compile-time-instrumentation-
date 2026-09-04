← [Technical Risks](13-technical-risks.md) · [Contents](../README.md) · [Final Recommendation](15-final-recommendation.md) →

---

## 14. Evaluation Plan


No numbers are invented anywhere in this section. Every cell in every results table below is to be filled by measurement.

### 14.1 Correctness

**Instrumentation coverage**
- Metric: instrumented functions ÷ instrumentable functions (per §12.2/§12.3 definitions), per crate.
- Method: compare `--plan-only` output against a hand-audited ground-truth list for a small corpus.
- Also record the *skip reason distribution* — how many functions were skipped for each rule in §12.3. A tool skipping 80% of functions for reasons nobody expected is a finding.

**Span correctness**
- Structural: for a known call graph, assert exact parent/child relationships in the exported spans.
- Naming: span names match the fully-qualified function path.
- Attributes: file, line, module path present and correct.
- Error propagation: a function returning `Err` produces a span with error status.
- Panic: under `panic=unwind`, the span closes; under `panic=abort`, document that it does not.

**Async behaviour** — the most important correctness axis
- Duration fidelity: `async fn` awaiting a known sleep of *D* produces total duration ≈ *D*. **[Revised, [Appendix D.2](appendix-d-maintainer-qa.md)]** The `busy ≈ 0` half is dropped — `busy`/`idle` is a `tracing-opentelemetry` synthesis with no field in the OTel data model. Replaced by: **while the task is suspended, its context is not current on the polling thread** (an unrelated instrumented function running on that thread must not become a child).
- Multi-poll: a future polled N times produces exactly **one** span, not N.
- Concurrency isolation: K concurrent tasks produce K independent span trees with no cross-contamination. This is the test that catches the held-guard-across-await bug.
- Suspension across threads: a task migrated between Tokio worker threads keeps one coherent span.
- Cancellation: a future dropped before completion closes its span (and we must decide and document whether it is marked cancelled).

**Context propagation**
- Phase 1: in-process parent/child across sync and async boundaries.
- Phase 2: `tokio::spawn` — parent context follows the spawned task.
- Phase 2+: cross-process via W3C `traceparent`, verified with a two-service test harness.

### 14.2 Performance

Every measurement takes an **uninstrumented baseline** in the same run.

**[Revised, [Appendix D.2](appendix-d-maintainer-qa.md)]** The configuration set changed with the move to the native OTel API: there is no `STATIC_MAX_LEVEL`, so "compiled out" is now the `--cfg` gate, and a new configuration (C) isolates the cost that replaces it ([R23](13-technical-risks.md)).

| Configuration | What it isolates |
| --- | --- |
| A. Uninstrumented | Baseline |
| B. Instrumented, `--cfg` gate **off** (call sites not spliced) | Should be ≈ A. If it is not, the gate is not doing its job |
| C. Instrumented, gate on, SDK **not recording** | **The cost that replaces `STATIC_MAX_LEVEL`** — a non-recording span where `tracing` would have compiled the site away (R23, [Appendix D.6](appendix-d-maintainer-qa.md) D-Q3) |
| D. Instrumented, recording, no exporter | Cost of span construction and attribute population alone |
| E. Instrumented, recording + OTLP to a local collector | Full realistic cost |
| F. Manual native-API instrumentation on the same functions | Are we worse than hand-written instrumentation? |
| G. Manual `#[tracing::instrument]` + `tracing-opentelemetry` on the same functions | The comparison a `tracing`-using reader will demand, and the empirical test of the D.2 reversal's cost (D-Q2) |

**Runtime / CPU**
- Microbenchmark (criterion): per-call cost of an instrumented sync fn and an instrumented async fn, across A–G.
- Macrobenchmark: throughput and p50/p95/p99 latency of a small HTTP service under load, across A–G.
- Report *distributions*, not means — instrumentation overhead is usually a tail problem.

**Memory**
- RSS at steady state across A–G.
- Allocation count/volume per request (via a counting allocator).

**Binary size**
- `.text` size and total stripped/unstripped size across A–G. B vs. C is the direct measure of what an always-present, non-recording call site costs in binary terms.

### 14.3 Build impact

**Expected range, from real data — [Appendix D.3](appendix-d-maintainer-qa.md).** `otelc`'s CodSpeed CI measures 5.3 s → 19.9 s (**+275%**) on a single-package baseline and 17.4 s → 26.8 s (**+54%**) multi-package: a large per-build fixed cost that amortises as the build grows. **Report our clean-build numbers against a 1.5×–3× expectation**, and treat a figure inside that band as normal rather than as a finding. Do not adopt `otelc`'s `BENCH_MAX_OVERHEAD_PCT=150` gate uncritically — their own single-package scenario exceeds it.

- Clean build wall time, A vs E.
- Incremental build after a one-line change, A vs E. **[Inference]** This is likely the worst-affected metric and the one users will complain about first — and unlike clean-build cost, it has no amortisation to hide behind. It is also the number [§15.6](15-final-recommendation.md) names as an abandon signal (consistently >2×).
- Peak memory during compilation.
- Time attributable to the tool itself (analysis + parse + rewrite + print) vs. to `rustc`, instrumented separately so we know which half to optimise.
- Cache effectiveness: second build with no changes should be near-instant; if it is not, R1/R9 are in play.

### 14.4 Comparison matrix (long-term)

To be filled only with measured values.

| Approach | Setup effort | Coverage of user code | Coverage of dependencies | Async span correctness | Cross-process propagation | Runtime overhead | Build overhead | Binary size | Privileges required | Platforms | Rebuild required |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| No instrumentation | — | none | none | — | — | baseline | baseline | baseline | none | all | — |
| Manual native OTel API | high | selective | none | good | manual | ? | ? | ? | none | all | yes |
| Manual `tracing` + bridge | high | selective | none | good | manual | ? | ? | ? | none | all | yes |
| **Compile-time (ours)** | low | broad | broad (Phase 2) | ? | Phase 2+ | ? | 1.5–3× clean **[Appendix D.3]** | ? | none | **all — incl. macOS, Windows, unprivileged containers** | yes |
| eBPF (OBI, today) | medium | network boundaries only | n/a | n/a | network-level only **[Fact]** | ? | none | none | `CAP_BPF` etc. **[Fact]** | Linux only | no |
| eBPF (OBI + #1096) | medium | **[pending upstream]** | n/a | **prototype exists** [Appendix D.4] | ? | ? | none | none | `CAP_BPF` etc. | Linux only | no |
| ~~Compiler-assisted eBPF~~ | — | — | — | — | — | — | — | — | — | — | **Row removed — architecture abandoned ([Appendix D.4](appendix-d-maintainer-qa.md))** |

**[Inference — updated]** The columns that decide adoption are *coverage of dependencies*, *async span correctness*, and *build overhead* — not runtime overhead, which every approach can make small. **[Added]** With the eBPF branch closed, the *platforms* column stops being a footnote and becomes the positioning: OBI #1096 will likely beat us on Linux-with-privileges, and cannot compete at all on macOS, Windows, unprivileged containers, or non-root deployments. Benchmark honestly against it there, and do not claim the ground it will own.

### 14.5 Methodology guardrails

- Fixed hardware, pinned toolchain, pinned dependency versions; record all three with every result.
- ≥10 runs per configuration; report median and IQR.
- Publish the harness alongside the numbers.
- Never publish a comparison against another project without running both ourselves.
- Explicitly record negative results. "Build time increased 40% on incremental builds" is a finding, not a failure to hide.

### 14.6 Continuous compatibility testing (the `otelc` #406 pattern)

**[Adopted from `otelc` maintainer Xabier Martinez — see [Appendix D.3](appendix-d-maintainer-qa.md).]** Evaluation is not only a one-time measurement exercise; the tool's correctness decays as the crates it instruments release new versions. `otelc` handles this with a scheduled CI workflow (`.github/workflows/test-latestlibrun.yaml`, issue #406) that fetches each instrumented library's latest stable release, runs the instrumentation tests against it, and auto-files a tracking issue (e.g. #565) when a private API changes and a version range must be split.

The Cargo port, to be in place before we ship rules for more than a handful of crates:

| Step | Mechanism |
| --- | --- |
| Discover | Query the crates.io index for the latest stable version of every crate we ship rules for, plus `opentelemetry`/`opentelemetry_sdk`/`opentelemetry-otlp` themselves ([R13](13-technical-risks.md)) |
| Build | Run the instrumented build and the §14.1 telemetry-correctness suite against that version, in the isolated `--target-dir` |
| Report | On failure, auto-file a tracking issue naming the crate, the version that broke, and the rule whose range needs splitting |
| Record | Keep the pass/fail matrix as published output — a rule's *tested* version range, not its *declared* one |

**[Inference]** Two reasons this ranks higher for us than it does for `otelc`. First, we splice into source rather than matching exported API shapes, so we are sensitive to internal refactors that do not change a crate's public API at all — a strictly larger breakage surface. Second, `otelc` is a SIG with contributors; an unattended rule set is worse for a small project, not better. This is the cheapest available substitute for people.

---

---

← [Technical Risks](13-technical-risks.md) · [Contents](../README.md) · [Final Recommendation](15-final-recommendation.md) →
