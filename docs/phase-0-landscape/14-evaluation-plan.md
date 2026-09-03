← [Technical Risks](13-technical-risks.md) · [Contents](README.md) · [Final Recommendation](15-final-recommendation.md) →

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
- Duration fidelity: `async fn` awaiting a known sleep of *D* produces total duration ≈ *D* and busy time ≈ 0.
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

| Configuration | What it isolates |
| --- | --- |
| A. Uninstrumented | Baseline |
| B. Instrumented, `STATIC_MAX_LEVEL` off | Cost of code that is compiled out — should be ≈ A |
| C. Instrumented, spans enabled, no subscriber | Cost of span construction alone |
| D. Instrumented, subscriber + OTLP to a local collector | Full realistic cost |
| E. Manual `#[instrument]` on the same functions | Are we worse than hand-written instrumentation? |

**Runtime / CPU**
- Microbenchmark (criterion): per-call cost of an instrumented sync fn and an instrumented async fn, across A–E.
- Macrobenchmark: throughput and p50/p95/p99 latency of a small HTTP service under load, across A–E.
- Report *distributions*, not means — instrumentation overhead is usually a tail problem.

**Memory**
- RSS at steady state across A–E.
- Allocation count/volume per request (via a counting allocator).

**Binary size**
- `.text` size and total stripped/unstripped size across A–E.

### 14.3 Build impact

- Clean build wall time, A vs D.
- Incremental build after a one-line change, A vs D. **[Inference]** This is likely the worst-affected metric and the one users will complain about first.
- Peak memory during compilation.
- Time attributable to the tool itself (analysis + parse + rewrite + print) vs. to `rustc`, instrumented separately so we know which half to optimise.
- Cache effectiveness: second build with no changes should be near-instant; if it is not, R1/R9 are in play.

### 14.4 Comparison matrix (long-term)

To be filled only with measured values.

| Approach | Setup effort | Coverage of user code | Coverage of dependencies | Async span correctness | Cross-process propagation | Runtime overhead | Build overhead | Binary size | Privileges required | Rebuild required |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| No instrumentation | — | none | none | — | — | baseline | baseline | baseline | none | — |
| Manual `tracing` | high | selective | none | good | manual | ? | ? | ? | none | yes |
| **Compile-time (ours)** | low | broad | broad (Phase 2) | ? | Phase 2+ | ? | ? | ? | none | yes |
| eBPF (OBI) | medium | network boundaries only | n/a | n/a | network-level only **[Fact]** | ? | none | none | `CAP_BPF` etc. **[Fact]** | no |
| Compiler-assisted eBPF | ? | ? | ? | **H2** | ? | ? | ? | ? | `CAP_BPF` etc. | no (for attach) |

**[Inference]** The columns that will actually decide adoption are *coverage of dependencies*, *async span correctness*, and *build overhead* — not runtime overhead, which every approach can make small. Design and benchmark accordingly.

### 14.5 Methodology guardrails

- Fixed hardware, pinned toolchain, pinned dependency versions; record all three with every result.
- ≥10 runs per configuration; report median and IQR.
- Publish the harness alongside the numbers.
- Never publish a comparison against another project without running both ourselves.
- Explicitly record negative results. "Build time increased 40% on incremental builds" is a finding, not a failure to hide.

---

---

← [Technical Risks](13-technical-risks.md) · [Contents](README.md) · [Final Recommendation](15-final-recommendation.md) →
