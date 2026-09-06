← [Appendix C - Adversarial Review](appendix-c-adversarial-review.md) · [Contents](../../README.md) · [Appendix E - Experiment Matrix](appendix-e-experiment-matrix.md) →

---

## Appendix D: Maintainer Q&A round (2026-09-04)

Following the adversarial review in [Appendix C](appendix-c-adversarial-review.md), we took our core technical assumptions directly to maintainers in three OpenTelemetry special interest groups: `#otel-rust`, `#otel-go`, and `#otel-ebpf`. Three of our central conclusions did not hold up. This appendix records the questions we asked, what the maintainers told us, and the architectural changes we made in response.

Appendix B tested questions against our local toolchain, and Appendix C evaluated them against primary source code. This round checked them with the people actively writing and maintaining the libraries. That conversation surfaced details that were impossible to find just by reading the repositories, including unmerged prototypes described in D.3 and D.4.

---

### D.1 Scorecard

| Prior claim in this document | Verdict | Source |
| --- | --- | --- |
| Only `tracing` handles async future interleaving correctly (§5.3, §5.4 reason 1) | **Refuted** | Scott Gerring (`#otel-rust`); `opentelemetry::trace::FutureExt::with_context` |
| Generating `tracing` spans is the right MVP target (§5.4) | **Overturned (decision reversed)** | D.2 below; we now adopt upstream guidance in `docs/traces.md` rather than diverging from it |
| `tracing-opentelemetry` is a mature, safe dependency to build on (§5.4 reason 6, §11.3) | **Qualified** | Scott Gerring: its context-sync bridge is "super hairy / hot-path-y / full of terror" |
| `otelc` publishes no overhead numbers at all (§2.9) | **Partly corrected** | Xabier Martinez (`#otel-go`): CodSpeed compile-time benchmarks exist; runtime latency benchmarks do not |
| H2 (async span reconstruction from poll events) is open and unclaimed (§7.5, §15.5 Q8) | **Resolved upstream (branch closed)** | Giuseppe Ognibene (`#otel-ebpf`): working prototype tracked under OBI #1096 |
| OBI has no application-level uprobes for Rust (§7.5, §1.2) | **Confirmed** | Nikola Grcevski (`#otel-ebpf`): falls back to generic socket kprobes |

---

### D.2 OpenTelemetry Rust: reversing the tracing vs. native API decision

**The question asked.** Can the native OpenTelemetry API correctly represent async future interleaving (where a future is polled, yields, and resumes on another thread), or does that strictly require the "entered" span concept from `tracing`? Sections 5.3 and 5.4 originally argued that only `tracing` handled this properly, which served as our primary reason for targeting it.

**Finding: The original assertion was incorrect.**
OpenTelemetry Rust already provides this exact mechanism. `opentelemetry::trace::FutureExt` exports `with_context(cx)` and `with_current_context()`, which wrap any `Future`. The wrapper attaches the context at the start of each `poll()` and detaches it as soon as the future yields. As a result, the active context follows the task to whichever thread polls it, and stays detached while the task is idle. This achieves the same runtime lifecycle as `tracing::Instrument` through a direct native path.

Scott Gerring (`#otel-rust`) put it directly:

> "If your goal is compile-time instrumentation, I don't see async future handling as a reason to favour the tracing api."

Scott also pointed out that the context synchronization bridge in `tracing-opentelemetry` is "super hairy / hot-path-y / full of terror." Section 5.4 had viewed that crate as a convenient buffer against OpenTelemetry API changes. In practice, it is a complex component on the hot path that introduces unnecessary risk.

**Decision: We dropped `tracing` code generation and now target the native OpenTelemetry API directly.**

Our compiler wrapper will wrap asynchronous futures using `FutureExt::with_context` instead of injecting `#[tracing::instrument]`. This change brings several clear advantages:

1. **Follows upstream recommendations.** The official `docs/traces.md` recommends: "For new code, prefer the OpenTelemetry Tracing API directly." Our earlier plan recorded an intentional disagreement based on async handling. With that technical objection resolved, our reason to diverge disappeared.
2. **Eliminates three heavy dependencies.** We remove `tracing`, `tracing-subscriber`, and `tracing-opentelemetry` from the injected dependency set. That closes [R14](13-technical-risks.md) (the version mismatch risk between `tracing-opentelemetry` and `opentelemetry`) and removes the hot-path synchronization bridge altogether.
3. **Restores full OpenTelemetry features.** Span kinds (`Server`, `Client`, `Internal`) and span links become native calls rather than workarounds mapped through special `otel.*` fields, or lost entirely as was the case for links ([Appendix C.1](appendix-c-adversarial-review.md)).

**Tradeoffs and new considerations:**
`tracing` supports `STATIC_MAX_LEVEL`, which strips disabled spans out of the binary during compilation. The native OpenTelemetry API has no direct equivalent; generated call sites remain in the binary, and disabled spans rely on a runtime check instead of compile-time elimination. Section 5.4 originally counted that as a point in favor of `tracing`. However, [Appendix C.1](appendix-c-adversarial-review.md) showed that `STATIC_MAX_LEVEL` is global and additive across dependencies, so it was never a dependable kill switch for our tool specifically. We already needed a dedicated `--cfg` flag to gate generated code. Splicing code behind `--cfg` achieves compile-time deletion regardless of which API is called. We track this runtime check behavior under [R23](13-technical-risks.md).

Targeting native OpenTelemetry also shifts our API stability risk. The traces API is currently Beta, and our code generator now calls it directly without an intermediate bridge to absorb breaking changes. We have rewritten the mitigation in [R13](13-technical-risks.md) to reflect this.

---

### D.3 OpenTelemetry Go (otelc): benchmark data and version compatibility

Xabier Martinez (`#otel-go`) shared internal CI benchmark numbers and explained how the Go project manages library compatibility across releases.

**Compile-time overhead:**
`otelc`'s published benchmarks (`BenchmarkCompile`) evaluate compilation time using CodSpeed, but they do not measure application request latency at runtime. Runtime latency benchmarks are scheduled for when instrumentation rules move to a separate repository.

| Scenario | Plain `go build` | With `otelc` | Overhead |
| --- | --- | --- | --- |
| Baseline (single package) | 5.3 s | 19.9 s | **+275%** |
| Multi-package | 17.4 s | 26.8 s | **+54%** |

This updates our note in [§2.9](02-otelc-go.md). While `docs/benchmarking.md` contained no numbers, real benchmarks do exist in CI. The data also clarifies the `BENCH_MAX_OVERHEAD_PCT=150` CI check: because the single-package baseline exceeds that ceiling, the check does not apply to every test scenario.

These figures show fixed costs versus marginal costs. Instrumentation setup has a baseline overhead per build, so it heavily impacts a quick 5-second build and amortizes across larger multi-package builds. For our Rust tool, we should anticipate approximately 1.5x to 3x clean compile times when instrumenting multiple crates, with the largest percentage impact on small crates. Section 14.3 should evaluate our performance against that realistic baseline.

**Automated compatibility testing:**
`otelc` tracks upstream library updates with a dedicated workflow (`.github/workflows/test-latestlibrun.yaml`, issue #406). The job pulls the latest stable release of each supported library from the Go module proxy, runs tests against it, and automatically files tracking issues (such as #565) whenever an internal API changes and requires an updated version rule.

**What we adopted:**
We can apply this exact pattern in Cargo. A scheduled CI job queries the crates.io index for the latest stable version of each supported dependency, tests our instrumented build against it, and opens a tracking issue if a build fails. This is the only practical way to handle [R16](13-technical-risks.md) (rewriting third-party code) across many dependencies, because hardcoded version ranges quietly break over time. See [§14.6](14-evaluation-plan.md).

---

### D.4 OpenTelemetry eBPF (OBI): upstream progress and closing the eBPF branch

We spoke with Nikola Grcevski and Giuseppe Ognibene (`#otel-ebpf`) regarding Rust support in OBI.

**Uprobe status:**
OBI currently has no application-level uprobes for Rust, falling back to generic socket kprobes. This confirms the status reported in [§7.5](07-ebpf-future.md) and [Appendix B](appendix-b-verification-log.md) item 6.

**Tokio async support:**
Giuseppe Ognibene built a working prototype for Tokio async task reconstruction and context propagation in eBPF, tracked under OBI issue #1096 ("Rust Tokio context propagation"). It is undergoing validation against two difficult edge cases: task migration across worker threads, and pointer reuse after tasks are dropped (the problem identified in [Appendix C.9](appendix-c-adversarial-review.md) Q3).

**Resolving hypothesis H2:**
Section 7.5 framed hypothesis H2 (that logical async spans can be reconstructed at runtime from poll-level uprobe events) as the core question for the eBPF branch, and Section 15.5 scheduled an exploratory prototype to test it. We no longer need to build that prototype ourselves, because the upstream maintainers have already built it directly inside OBI.

**Pivot condition triggered:**
Section 15.6 established that we would step back from building our own eBPF branch if upstream OBI added semantic Rust function instrumentation. That condition is now met. Rather than building a parallel implementation with less familiarity with OBI internals, we can collaborate with the upstream project.

**Project changes:**
- **Architectures C and D are closed.** We will not build an eBPF loader or sidecar agent. The designs remain documented in [§10](10-architecture-candidates.md) for historical reference.
- **We avoid duplicate work.** We will not write our own Tokio reconstruction layer.
- **We collaborate upstream.** When Giuseppe's pull request opens, we can review it and share our analysis on Rust async semantics from [§6.3](06-rust-specific-challenges.md) and [§7](07-ebpf-future.md).
- **We focus entirely on Architecture A (compile-time Cargo instrumentation).** This gives our tool a distinct, practical role: it supports environments where eBPF cannot run, including macOS, Windows, unprivileged containers, and non-root host deployments.

This resolves the exploratory research branch from [§15.1](15-final-recommendation.md). We received a definitive answer early, avoided spending development time on a parallel implementation, and can focus our efforts where compile-time instrumentation is uniquely needed.

---

### D.5 Summary of changes

| # | Change | Driver | Affected |
| --- | --- | --- | --- |
| 1 | Code generation targets native OTel API calls using `FutureExt::with_context` rather than `#[tracing::instrument]` | D.2 | [§5.4](05-otel-rust.md), [§10](10-architecture-candidates.md), [§11.1](11-recommended-architecture.md), [§12.1](12-mvp-definition.md), [§15.2](15-final-recommendation.md) |
| 2 | `tracing`, `tracing-subscriber`, and `tracing-opentelemetry` removed from the injected dependency set | D.2 | [§12.4](12-mvp-definition.md) |
| 3 | R14 (`tracing-opentelemetry` version offset) closed as obsolete | D.2 | [R14](13-technical-risks.md) |
| 4 | R13 (Beta traces API churn) mitigation rewritten to address direct API dependency | D.2 | [R13](13-technical-risks.md) |
| 5 | R15 (#1571 resolving against `tracing`) closed as moot | D.2 | [R15](13-technical-risks.md) |
| 6 | New risk R23: reliance on runtime checks rather than `STATIC_MAX_LEVEL` compile-time span removal | D.2 | [R23](13-technical-risks.md) |
| 7 | eBPF branch (Architectures C and D) closed per Section 15.6 pivot condition | D.4 | [§7](07-ebpf-future.md), [§10](10-architecture-candidates.md), [§15.4](15-final-recommendation.md), [§15.6](15-final-recommendation.md) |
| 8 | H2 and Q8 closed as resolved upstream | D.4 | [§7.5](07-ebpf-future.md), [§15.5](15-final-recommendation.md) |
| 9 | Risks R19 to R21 (eBPF portability, H2 falsity, eBPF scope creep) closed | D.4 | [R19–R21](13-technical-risks.md) |
| 10 | Build-overhead expectation set to 1.5x to 3x clean compile time based on `otelc` measurements | D.3 | [§14.3](14-evaluation-plan.md) |
| 11 | Automated dependency compatibility CI adopted from the Go #406 model | D.3 | [§14.6](14-evaluation-plan.md) |
| 12 | Trampoline symbol names standardized on `__otel_span_enter` and `__otel_span_exit` | Consistency with [Appendix C.2](appendix-c-adversarial-review.md) | [§10](10-architecture-candidates.md), [§12.4](12-mvp-definition.md) |

---

### D.6 Open questions after this round

Closing the eBPF branch also resolved questions Q3, Q4, and Q5 from [Appendix C.9](appendix-c-adversarial-review.md), since those supported hypothesis H2.

The remaining open questions from that review, combined with the new findings from this round and [§16](16-instrumentation-semantics.md), are consolidated in [Appendix E.3](appendix-e-experiment-matrix.md) as twelve future experiments (mapping: D-Q1 to FE-5, D-Q2 to FE-6, D-Q3 to FE-12, D-Q4 to FE-8, D-Q5 to FE-1). The Phase 0 completion audit added four additional questions that emerged from specifying the generated code: FE-2 (Tier-2 async across the C ABI, [R25](13-technical-risks.md)), FE-3 (`#![forbid(unsafe_code)]`, [R26](13-technical-risks.md)), FE-4 (span start at construction versus first poll), and FE-7 (non-Windows link models, [R24](13-technical-risks.md)).

| # | Question | Status |
| --- | --- | --- |
| D-Q1 | Does `FutureExt::with_context` handle high span volumes reliably during task migration across Tokio worker threads? | **New.** The method is documented and maintainer-approved, but needs measurement under automated compiler span volume. Becomes MVP success criteria 4 and 5 ([§12.7](12-mvp-definition.md)). |
| D-Q2 | What is the runtime cost per span using native OpenTelemetry compared to `tracing` and `tracing-opentelemetry` for short-lived functions? | **New.** Supersedes [Appendix C.9](appendix-c-adversarial-review.md) Q6, which assumed `tracing` as the baseline. |
| D-Q3 | Without `STATIC_MAX_LEVEL`, what is the runtime overhead of a disabled span when the `--cfg` gate is enabled but the SDK is not recording? | **New.** Gating by `--cfg` eliminates call sites at compile time; this question measures the cost when generated code remains in the binary ([R23](13-technical-risks.md)). |
| D-Q4 | Does source mirroring work reliably across real-world multi-file crates (`include!`, `#[path]`, modules generated by `build.rs`, `sqlx::query!`)? | **Top priority.** Carried forward from [Appendix C.9](appendix-c-adversarial-review.md) Q1 and Q2. With eBPF closed, this is our primary open technical question. |
| D-Q5 | Do injected `extern "C"` trampolines compile cleanly with `lto = true`, `codegen-units = 1`, and `panic = "abort"`? | **Carried forward.** From [Appendix C.9](appendix-c-adversarial-review.md) Q7; test case defined in [§12.8](12-mvp-definition.md). |

---

← [Appendix C - Adversarial Review](appendix-c-adversarial-review.md) · [Contents](../../README.md) · [Appendix E - Experiment Matrix](appendix-e-experiment-matrix.md) →
