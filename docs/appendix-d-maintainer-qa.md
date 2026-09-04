← [Appendix C — Adversarial Review](appendix-c-adversarial-review.md) · [Contents](../README.md)

---

## Appendix D — Maintainer Q&A round (2026-09-04)

Following the adversarial review round ([Appendix C](appendix-c-adversarial-review.md)), direct questions were put to core maintainers across three OpenTelemetry SIGs: `#otel-rust`, `#otel-go`, and `#otel-ebpf`. Three of this document's load-bearing conclusions did not survive. This appendix records what was asked, what was answered, and what changed.

**Why this round outranks the previous two.** Appendix B settled questions by experiment against the local toolchain; Appendix C settled them against primary source code. This round settles them against the people who maintain the code — including one answer (D.3) that no amount of reading could have produced, because the work in question is unmerged.

---

### D.1 Scorecard

| Prior claim in this document | Verdict | Source |
| --- | --- | --- |
| Only `tracing` handles async future interleaving correctly (§5.3, §5.4 reason 1) | **Refuted** | Scott Gerring (`#otel-rust`); `opentelemetry::trace::FutureExt::with_context` |
| Generating `tracing` spans is the right MVP target (§5.4) | **Overturned — decision reversed** | D.2 below; upstream `docs/traces.md` guidance now adopted rather than diverged from |
| `tracing-opentelemetry` is a mature, safe dependency to build on (§5.4 reason 6, §11.3) | **Qualified** | Scott Gerring: its context-sync bridge is *"super hairy / hot-path-y / full of terror"* |
| `otelc` publishes no overhead numbers at all (§2.9) | **Partly corrected** | Xabier Martinez (`#otel-go`): CodSpeed compile-time benchmarks exist; runtime latency benchmarks genuinely do not |
| H2 (async span reconstruction from poll events) is open and unclaimed (§7.5, §15.5 Q8) | **Resolved upstream — branch closed** | Giuseppe Ognibene (`#otel-ebpf`): working prototype, tracked under OBI #1096 |
| OBI has no application-level uprobes for Rust (§7.5, §1.2) | **Confirmed** | Nikola Grcevski (`#otel-ebpf`) — falls back to generic socket kprobes |

---

### D.2 OpenTelemetry Rust: the `tracing` vs. native-API decision is overturned

**The question asked.** Whether async future interleaving — a span whose future is polled, suspended, resumed on a different thread — can be represented correctly using the native OpenTelemetry API, or whether `tracing`'s separate "entered" concept is genuinely required. §5.3 and §5.4 reason 1 of this document asserted the latter, and made it the primary justification for the whole emitter decision.

**[Fact — the assertion was wrong.]** OpenTelemetry Rust already ships exactly this mechanism. `opentelemetry::trace::FutureExt` provides `with_context(cx)` and `with_current_context()`, which wrap any `Future`. The wrapper calls `attach()` on the context at the start of each `poll()` and detaches when the future yields — so the context is correctly present on whichever thread polls the future, and correctly absent while the task is suspended. This is the same lifecycle `tracing::Instrument` implements, reached by a different route.

**Maintainer, Scott Gerring (`#otel-rust`):**

> "If your goal is compile-time instrumentation, I don't see async future handling as a reason to favour the tracing api."

**[Fact]** Scott separately characterised `tracing-opentelemetry`'s context synchronisation bridge — the component §5.4 reason 6 credited with absorbing OTel API churn on our behalf — as *"super hairy / hot-path-y / full of terror."* That reframes it from an asset we inherit for free into a hot-path component we would be taking a dependency on without understanding it.

**Decision — reversed. We abandon generating `tracing` spans as the primary target and generate native OpenTelemetry API calls directly.**

Generated async instrumentation wraps the future via `FutureExt::with_context` rather than emitting `#[tracing::instrument]`. What this buys:

1. **Alignment with upstream, not divergence from it.** `docs/traces.md` says *"For new code, prefer the OpenTelemetry Tracing API directly."* §5.4 previously recorded a considered disagreement with that guidance, resting largely on the async argument that D.2 has now removed. With the argument gone, the disagreement goes with it.
2. **Three dependencies eliminated.** `tracing`, `tracing-subscriber`, and `tracing-opentelemetry` all leave the injected dependency set. This deletes [R14](13-technical-risks.md) (the deliberate version offset between `tracing-opentelemetry` and `opentelemetry`) outright, and removes the hot-path synchronisation overhead of the bridge.
3. **Full OTel expressiveness restored.** Span kinds (`Server`, `Client`, `Internal`) and span links become first-class rather than bridged through `otel.*` magic fields or, in the case of links, unavailable entirely ([Appendix C.1](appendix-c-adversarial-review.md)).

**What this costs, stated plainly.** `tracing`'s `STATIC_MAX_LEVEL` compiles disabled spans out of the binary entirely; the native OTel API has no equivalent, so generated call sites are always present and the "off" path becomes a runtime check rather than a compile-time deletion. This was previously counted as an advantage of targeting `tracing` (§5.4 reason 4) and is now a real regression — tracked as [R23](13-technical-risks.md). It does not reverse the decision, because [Appendix C.1](appendix-c-adversarial-review.md) had already established that `STATIC_MAX_LEVEL` is global and additive and therefore never was the per-tool kill switch it was described as; a dedicated `--cfg` gate on generated code was already required either way, and a `--cfg`-gated splice is a compile-time deletion regardless of which API the spliced code calls.

**Second cost:** the churn argument reverses direction. The traces API is Beta, and we now generate against it directly with no bridge crate absorbing breaking changes — see [R13](13-technical-risks.md), whose mitigation is rewritten accordingly.

---

### D.3 OpenTelemetry Go (`otelc`): real benchmark and compatibility data

**Maintainer, Xabier Martinez (`#otel-go`)**, who shared internal CI benchmark data and the compatibility-testing workflow.

**[Fact — compile-time overhead, measured via CodSpeed.]** `otelc`'s published benchmarks measure compile time only (`BenchmarkCompile`); they do not measure application runtime request latency. Per-library runtime latency benchmarking is deferred until instrumentation rules are decoupled into a separate repository.

| Scenario | Plain `go build` | With `otelc` | Overhead |
| --- | --- | --- | --- |
| Baseline (single package) | 5.3 s | 19.9 s | **+275%** |
| Multi-package | 17.4 s | 26.8 s | **+54%** |

This partly corrects [§2.9](02-otelc-go.md), which recorded that `docs/benchmarking.md` contains no numbers — true of that file, but the CI benchmarks themselves do exist. It also puts the `BENCH_MAX_OVERHEAD_PCT=150` CI gate in context: the single-package baseline scenario runs well past that figure, so the gate is evidently not applied uniformly across all scenarios.

**[Inference]** The two numbers are not in tension — they are the fixed-cost and marginal-cost ends of the same curve. Instrumentation setup work is largely per-build, so it dominates a 5.3-second build and amortises across a 17.4-second one. **The projection for our tool: expect roughly 1.5×–3× clean compile time when instrumenting multiple crates, worst on small projects.** [§14.3](14-evaluation-plan.md) should report against that range rather than against an invented target.

**[Fact — automated forward-compatibility testing.]** `otelc` maintains compatibility across upstream library releases with a CI workflow (`.github/workflows/test-latestlibrun.yaml`, tracked under issue #406) that fetches the latest stable release of each instrumented library from the Go module proxy, runs the instrumentation test suite against it, and **auto-files tracking issues** (e.g. #565) when a private API changes and a rule's supported version range must be split.

**Adopted.** The same pattern ports directly to Cargo: query the crates.io index for the latest stable version of each crate we ship rules for, run the instrumented build against it on a schedule, and open a tracking issue on breakage. This is the only sustainable answer to [R16](13-technical-risks.md) (rewriting third-party sources) at more than a handful of crates — a rule pinned to a version range rots silently otherwise. See [§14.6](14-evaluation-plan.md).

---

### D.4 OpenTelemetry eBPF (OBI): H2 is resolved upstream, and the branch closes

**Maintainers, Nikola Grcevski and Giuseppe Ognibene (`#otel-ebpf`).**

**[Fact — confirmed]** OBI currently has **zero** application-level uprobes for Rust. Rust falls back to the generic socket-kprobe path. This confirms [§7.5](07-ebpf-future.md) and [Appendix B](appendix-b-verification-log.md) item 6 exactly as previously recorded.

**[Fact — new, and decisive]** Giuseppe Ognibene has a **working prototype** of Tokio async task reconstruction and context propagation in eBPF, tracked under **OBI issue #1096 ("Rust Tokio context propagation")**. It is in final testing against the edge cases this document independently predicted would be the hard ones: task migration across worker threads, and pointer reuse after a task is freed — the latter being precisely [Appendix C.9](appendix-c-adversarial-review.md) Q3.

**This resolves H2.** [§7.5](07-ebpf-future.md) framed H2 — *"logical async spans can be reconstructed at runtime from poll-level uprobe events"* — as the load-bearing hypothesis for the entire eBPF branch, and [§15.5](15-final-recommendation.md) Q8 scheduled a prototype to test it. That prototype no longer needs building: someone with deep OBI-internals knowledge has built it, upstream, in the project that would have consumed the result.

**[Decision — the §15.6 pivot condition is formally triggered.]** [§15.6](15-final-recommendation.md) states the eBPF branch should be abandoned if *"OBI ships semantic Rust function-level instrumentation upstream."* That condition has now fired in its strongest form: not a competitor shipping something adjacent, but the canonical upstream project actively building the exact capability, with the maintainer reachable and receptive.

**Consequences:**

- **Architectures C and D are abandoned.** No eBPF loader, no sidecar, no competing implementation. Both remain in [§10](10-architecture-candidates.md) as a research record, marked closed.
- **We do not build what #1096 builds.** Building a second Tokio-reconstruction implementation in parallel, with less OBI knowledge, would be a strictly worse use of the same effort.
- **We collaborate instead.** Review Giuseppe's upstream PR when it opens; contribute the Rust-side async semantics knowledge in [§6.3](06-rust-specific-challenges.md) and [§7](07-ebpf-future.md), which is directly relevant to the edge cases still in testing.
- **The project focuses 100% on Architecture A** — compile-time Cargo instrumentation — whose scope is now cleanly complementary rather than overlapping: **it serves the platforms where eBPF cannot run at all.** macOS, Windows, unprivileged containers, and any non-root deployment are unreachable by #1096 no matter how well it works, and that is a durable division of labour rather than a consolation prize.

**[Inference]** This is the best available outcome for the eBPF half, and it should be read as such rather than as a loss. The research question was real, it has been answered by someone better positioned to answer it, and the answer arrived before we spent a phase on it. [§15.1](15-final-recommendation.md)'s framing — *"one speculative research question attached as an optional later branch"* — is now simply resolved: the branch is closed, and the compile-time half, which always had standalone value, is the whole project.

---

### D.5 What changed as a result

| # | Change | Driver | Affected |
| --- | --- | --- | --- |
| 1 | Code-generation target moves from `#[tracing::instrument]` to native OTel API calls using `FutureExt::with_context` | D.2 | [§5.4](05-otel-rust.md), [§10](10-architecture-candidates.md), [§11.1](11-recommended-architecture.md), [§12.1](12-mvp-definition.md), [§15.2](15-final-recommendation.md) |
| 2 | `tracing`, `tracing-subscriber`, `tracing-opentelemetry` removed from the injected dependency set | D.2 | [§12.4](12-mvp-definition.md) |
| 3 | R14 (`tracing-opentelemetry` version offset) closed as no longer applicable | D.2 | [R14](13-technical-risks.md) |
| 4 | R13 (Beta traces API churn) mitigation rewritten — we are now directly exposed, not shielded by a bridge | D.2 | [R13](13-technical-risks.md) |
| 5 | R15 (#1571 resolving against `tracing`) closed — moot once we generate the OTel API | D.2 | [R15](13-technical-risks.md) |
| 6 | New R23: loss of `STATIC_MAX_LEVEL`-style compile-time span removal | D.2 | [R23](13-technical-risks.md) |
| 7 | eBPF branch (Architectures C, D) formally abandoned; §15.6 pivot condition executed | D.4 | [§7](07-ebpf-future.md), [§10](10-architecture-candidates.md), [§15.4](15-final-recommendation.md), [§15.6](15-final-recommendation.md) |
| 8 | H2 / Q8 closed as resolved upstream rather than open | D.4 | [§7.5](07-ebpf-future.md), [§15.5](15-final-recommendation.md) |
| 9 | R19–R21 (eBPF portability, H2 falsity, eBPF scope creep) closed | D.4 | [R19–R21](13-technical-risks.md) |
| 10 | Build-overhead expectation set at 1.5×–3× clean compile, from real `otelc` data | D.3 | [§14.3](14-evaluation-plan.md) |
| 11 | Automated latest-version compatibility CI adopted (the #406 pattern) | D.3 | [§14.6](14-evaluation-plan.md) |
| 12 | Trampoline symbol names standardised on `__otel_span_enter` / `__otel_span_exit` | Consistency with [Appendix C.2](appendix-c-adversarial-review.md) | [§10](10-architecture-candidates.md), [§12.4](12-mvp-definition.md) |

---

### D.6 Open questions after this round

Closing the eBPF branch closes [Appendix C.9](appendix-c-adversarial-review.md) Q3, Q4, and Q5 — they were all in service of H2. What this round leaves open, and what it newly opens:

| # | Question | Status |
| --- | --- | --- |
| D-Q1 | Does `FutureExt::with_context` behave correctly under task migration across Tokio worker threads, at the volume auto-instrumentation generates? | **New.** The mechanism is documented and maintainer-endorsed; its behaviour under our specific span volume is unmeasured. Becomes MVP success criteria 4 and 5 ([§12.7](12-mvp-definition.md)) |
| D-Q2 | What is the per-span cost of the native OTel API versus `tracing` + `tracing-opentelemetry`, for many small short-lived spans? | **New, and now cheaper to answer.** Supersedes [Appendix C.9](appendix-c-adversarial-review.md) Q6 (`fastrace` comparison), which was premised on `tracing` being the baseline |
| D-Q3 | Without `STATIC_MAX_LEVEL`, what does the generated-code kill switch actually cost when off? | **New.** A `--cfg` gate deletes the call site at compile time; the question is what a *runtime*-disabled span costs when the `--cfg` gate is on but the SDK is not recording ([R23](13-technical-risks.md)) |
| D-Q4 | Does source-tree mirroring survive real multi-file crates (`include!`, `#[path]`, `build.rs`-generated modules, `sqlx::query!`)? | **Unchanged and now top-priority.** [Appendix C.9](appendix-c-adversarial-review.md) Q1/Q2 — with the eBPF branch closed, this is the largest remaining unknown in the project |
| D-Q5 | Do injected `extern "C"` trampolines survive `lto = true`, `codegen-units = 1`, `panic = "abort"`? | **Unchanged.** [Appendix C.9](appendix-c-adversarial-review.md) Q7; regression test already specified in [§12.8](12-mvp-definition.md) |

---

← [Appendix C — Adversarial Review](appendix-c-adversarial-review.md) · [Contents](../README.md)
