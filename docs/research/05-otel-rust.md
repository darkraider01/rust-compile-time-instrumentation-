← [Existing Rust Instrumentation Landscape](04-rust-instrumentation-landscape.md) · [Contents](../../README.md) · [Rust-Specific Instrumentation Challenges](06-rust-specific-challenges.md) →

---

## 5. OpenTelemetry Rust - Current Architecture and What We Should Target

**[Decision reversed after the maintainer Q&A round - see [Appendix D.2](appendix-d-maintainer-qa.md).** This section previously recommended generating `tracing` instrumentation, resting primarily on the claim that only `tracing` models async future interleaving correctly. OpenTelemetry Rust maintainer Scott Gerring disproved that claim directly: `opentelemetry::trace::FutureExt::with_context` already wraps any `Future`, attaching the context on each `poll()` and detaching on yield across threads - the same lifecycle as `tracing::Instrument`. **The tool now generates native OpenTelemetry API calls.** §5.3 and §5.4 below are rewritten accordingly; §5.1 and §5.2 are unchanged as historical context, with their now-superseded conclusions marked.]**

### 5.1 Component maturity

**[Fact]** From the [opentelemetry-rust README](https://github.com/open-telemetry/opentelemetry-rust) at time of investigation:

| Component | Status |
| --- | --- |
| Logs API | **Stable** |
| Logs SDK | **Stable** |
| Metrics API | **Stable** |
| Metrics SDK | **Stable** |
| Traces API | **Beta** |
| Traces SDK | **Beta** |

**[Fact]** MSRV is 1.75; the project supports the current stable compiler plus the three preceding minor versions.

**[Inference - important and uncomfortable]** This is the inverse of Go, Java, and most other OTel implementations, where traces stabilised first. **The exact signal our tool would generate - spans - is the one part of OpenTelemetry Rust that is still Beta.** Practical consequences:

1. We must expect breaking changes in the traces API/SDK across `opentelemetry` minor versions during the project's life.
2. Any code we *generate* should keep its contact surface with the traces API as small as possible, so that an `opentelemetry` bump touches few generated constructs.
3. ~~This is a further argument for generating `tracing` calls: `tracing` 0.1.x has been API-stable for years, and the churn is absorbed by `tracing-opentelemetry` - a crate someone else maintains.~~ **[Superseded - [Appendix D.2](appendix-d-maintainer-qa.md).]** We now generate against the Beta traces API directly, with no bridge crate absorbing churn on our behalf. That is a real cost of the reversal and is why [R13](13-technical-risks.md)'s mitigation was rewritten: the answer is a tested version pair, a narrow generated-code surface (span start, attribute set, status set, `with_context`), and the emitter seam ([§11.4](11-recommended-architecture.md)) - not an intermediary crate. The offsetting gain is that the intermediary being removed is one its own maintainer describes as *"super hairy / hot-path-y / full of terror."*

### 5.2 The two-API problem - RESOLVED since the original research pass

**[Fact]** [open-telemetry/opentelemetry-rust#1571](https://github.com/open-telemetry/opentelemetry-rust/issues/1571) - "OpenTelemetry Tracing API vs Tokio-Tracing API for Distributed Tracing" - framed the problem as users being forced to choose between two competing span APIs:

- the `opentelemetry` crate's Tracing API, aligned with the cross-language OTel specification;
- the `tracing` crate, which predates OTel in Rust, has overwhelming ecosystem adoption, and is actively maintained.

**[Fact]** Problems cited: incomplete traces when different layers of an application use different APIs; log↔trace correlation breaking; and no comprehensively tested interoperability between them. `tracing-opentelemetry` was described as a mitigation, not a solution. Four options were enumerated - deprecate tokio-tracing (judged improbable given adoption); deprecate the overlapping OTel APIs and treat `tracing` as the Rust standard; maintain both with seamless interoperability; or stop accommodating each other.

**[Fact - resolved in the verification pass]** The issue is **closed** (`state_reason: completed`, closed 2026-03-18, 47 comments). The maintainers settled on **"Option 3 - Maintain Both APIs"** (per maintainer scottgerring, 2025-04-01): both the OTel-native API and `tracing` remain first-class, with the goal of fixing interoperability (the "context synchronisation issue" tracked separately in [#1690](https://github.com/open-telemetry/opentelemetry-rust/issues/1690)) rather than deprecating either. The closing comment (2026-03-18) states plainly: "Interop works now; unified context would be nice but seems unrealistic. Let's put a line under it." So `tracing` is **not** at risk of being deprecated in favour of the OTel API - both are permanent, sanctioned choices.

**[Fact - new finding, and this is the one that actually matters for §5.4]** The PR that closed out the issue's action items, [#3122](https://github.com/open-telemetry/opentelemetry-rust/pull/3122) ("docs: start doc for distributed tracing and logs guidance", merged), added `docs/traces.md` to the opentelemetry-rust repository. That document - the project's own current, official guidance, published *after* the two-API debate concluded - states:

> "For new code, prefer the OpenTelemetry Tracing API directly."

citing `tracing`'s lack of span kind, links, and remote-parent support as the reason. `docs/traces.md` is itself marked **Work-In-Progress** (traces remain Beta), while the companion `docs/logs.md` is marked Stable.

**[Resolved by the maintainer Q&A round - see [Appendix D.2](appendix-d-maintainer-qa.md).]** This paragraph previously recorded a "considered disagreement with upstream guidance": both APIs survive, but the maintainers advise the OTel API for new code, and we recommended `tracing` anyway. **That disagreement is now withdrawn.** Its load-bearing technical argument was async lifecycle handling, and that argument was refuted directly by a maintainer (§5.3). We follow `docs/traces.md` and generate the OpenTelemetry Tracing API. Note what does *not* change: #1571 resolved as "maintain both," so `tracing` is not deprecated and users who instrument their own code with it are not stranded - our generated spans and their `tracing` spans simply meet at the SDK rather than in one span tree, which is the same reconciliation any mixed-API Rust application already performs.

### 5.3 Span lifecycle, context propagation, semantic conventions

**Span lifecycle.** In `tracing`, a span is created, then *entered* and *exited* possibly many times, and finally closed when the last handle is dropped. Entering/exiting is not the same as starting/ending. `tracing-opentelemetry` maps a `tracing` span's full lifetime (creation → close) onto an OTel span's start → end; the multiple enter/exit pairs of an async span become `busy`/`idle` timing rather than separate spans.

**[Refuted - see [Appendix D.2](appendix-d-maintainer-qa.md).** The original text argued that this mapping is *the* reason `tracing` is the right target for async Rust, and that reproducing it on the OTel API "would mean rebuilding `tracing-opentelemetry`." That is wrong, and it was the single most consequential error in this document.]**

**[Fact]** The OpenTelemetry Rust API already ships the equivalent mechanism. `opentelemetry::trace::FutureExt` provides `with_context(cx)` and `with_current_context()`, which wrap any `Future` and:

- call `attach()` on the context at the start of every `poll()`, so the span's context is current on whichever worker thread happens to poll it;
- detach when the future yields, so the context is *not* current while the task is suspended and cannot leak into unrelated work on that thread.

That is the same enter-on-poll / exit-on-yield lifecycle `tracing::Instrument` implements. The distinction that mattered - "an OTel span has one start and one end, but an async function's execution is interleaved" - is real, and both crates answer it the same way: **the span's start and end bracket the whole logical operation, while context attachment tracks the interleaving.** Nothing needs rebuilding.

**Maintainer, Scott Gerring (`#otel-rust`):** *"If your goal is compile-time instrumentation, I don't see async future handling as a reason to favour the tracing api."*

**[Inference]** What is genuinely lost by not going through `tracing` is the `busy`/`idle` split, which `tracing-opentelemetry` synthesises from enter/exit pairs and the OTel data model has no field for. That is a nice-to-have diagnostic, not a correctness property - and §5.4's original framing treated its absence as *silently wrong span durations*, which was never true of `with_context`.

**Generated form.** An instrumented `async fn` therefore wraps its body's future in `.with_context(cx)` rather than receiving an `#[instrument]` attribute. A synchronous function keeps the RAII-guard shape, where a guard held to end-of-scope is correct precisely because there is no suspension point to hold it across ([R5](13-technical-risks.md) is unchanged in substance: never hold an attach guard across an `.await`, in either API).

**Context propagation.** OTel Rust provides `Context`, `TextMapPropagator`, and a W3C TraceContext propagator; injection/extraction at process boundaries is manual - you call `propagator.inject_context(...)` on outbound requests and `extract(...)` on inbound ones. **[Fact]** `tracing-opentelemetry`'s `OpenTelemetrySpanExt` exposes `set_parent`/`context` so a remote parent can be attached to a `tracing` span.

**[Inference]** Cross-process propagation is where automatic instrumentation earns most of its value and where it is hardest: it requires knowing the *type* of the outbound request object (a `reqwest::RequestBuilder`, a `tonic` request, a `hyper::Request`) in order to inject headers. This is a *library-specific* rule, not a generic function rule - which is precisely why `otelc` shipped `net/http` and gRPC rules rather than a generic mechanism, and why our Phase 1 must not promise distributed propagation.

**Semantic conventions.** The `opentelemetry-semantic-conventions` crate provides generated constants (`HTTP_REQUEST_METHOD`, `SERVER_ADDRESS`, `DB_SYSTEM_NAME`, …). **[Inference]** Generic function instrumentation cannot produce semantic-convention-compliant attributes - there is no convention for "function `foo::bar` was called." Semantic conventions only apply to *library-specific* rules. Phase 1 should therefore emit `code.*`-style attributes (function name, module path, file, line) and explicitly not claim semantic-convention compliance.

**[Fact - resolved in the verification pass]** The `code.*` attribute group was promoted to **Stable** in semantic conventions v1.33.0. The stable, current attribute names (renamed from earlier experimental names) are: `code.function.name` (fully-qualified name, without arguments - replaces the old `code.namespace` + function-name split), `code.file.path` (replaces `code.filepath`), `code.line.number` (replaces `code.lineno`), `code.column.number`, and `code.stacktrace`. These are the attribute names Phase 1 should emit; see [opentelemetry.io/docs/specs/semconv/registry/attributes/code/](https://opentelemetry.io/docs/specs/semconv/registry/attributes/code/) and the [migration guide](https://opentelemetry.io/docs/specs/semconv/non-normative/code-attrs-migration/) for instrumentation authors migrating from the old names.

### 5.4 Decision: what should the tool generate?

> **Should compiler instrumentation generate `tracing` instrumentation, directly use OpenTelemetry APIs, or use another abstraction?**

**Recommendation - REVERSED after the maintainer Q&A round ([Appendix D.2](appendix-d-maintainer-qa.md)): generate native OpenTelemetry API calls. Wrap instrumented futures with `opentelemetry::trace::FutureExt::with_context`. Do not generate `tracing` spans. Do not invent a third abstraction.**

**This recommendation now follows the OpenTelemetry Rust project's own published guidance** ("For new code, prefer the OpenTelemetry Tracing API directly," `docs/traces.md`, §5.2) rather than diverging from it.

**Why it reversed.** The previous recommendation rested on six reasons. The first was load-bearing and is now refuted; the rest do not carry the decision without it.

| # | Original reason for `tracing` | Status after [Appendix D.2](appendix-d-maintainer-qa.md) |
| --- | --- | --- |
| 1 | Async semantics are solved in `tracing` "and nowhere else"; raw OTel spans would silently include suspended time in their duration | **Refuted, by a maintainer, on primary evidence.** `FutureExt::with_context` attaches on each `poll()` and detaches on yield (§5.3). Scott Gerring: *"I don't see async future handling as a reason to favour the tracing api."* The claimed silent-wrongness never applied to `with_context` |
| 2 | The traces API is Beta; `tracing` 0.1 is not | **Still true, and now a cost we accept.** It is a version-pinning problem ([R13](13-technical-risks.md)), not a correctness one. Reason 1 was the only correctness argument |
| 3 | Ecosystem convergence - self-instrumented libraries emit `tracing` spans, so ours nest with theirs for free | **Weakened.** Real, but it cuts both ways: mixed-API applications already reconcile at the SDK, and the reconciliation `tracing-opentelemetry` performs to make that "free" nesting work is the very component its maintainer calls *"super hairy / hot-path-y / full of terror"* |
| 4 | Compile-time removability via statically disabled levels | **Mostly already withdrawn in [Appendix C.1](appendix-c-adversarial-review.md)** - `STATIC_MAX_LEVEL` is global and additive, so it was never a per-tool kill switch, and a dedicated `--cfg` gate was required regardless. The residual real loss is tracked as [R23](13-technical-risks.md) |
| 5 | Debuggability - a generated `#[instrument]` attribute is readable | **Neutral.** A generated `let _span = tracer.start(...)` / `.with_context(cx)` is equally readable, and unlike an attribute macro it needs no `cargo expand` to see what it does |
| 6 | `tracing-opentelemetry` absorbs OTel API churn for us | **Inverted.** We were treating an unfamiliar hot-path component as free maintenance relief. Removing it removes both the churn shield and the hazard; see reason 2 for how the churn is handled instead |

**What the reversal buys:**

1. **Alignment with upstream guidance** instead of a documented disagreement with it (§5.2).
2. **Three fewer injected dependencies** - `tracing`, `tracing-subscriber`, `tracing-opentelemetry` all leave the injected set ([§12.4](12-mvp-definition.md)). This closes [R14](13-technical-risks.md) (the deliberate version offset between `tracing-opentelemetry` and `opentelemetry`, an active resolution hazard for a tool that injects both) and removes the bridge's hot-path context synchronisation.
3. **Full OTel expressiveness.** Span kinds (`Server`, `Client`, `Internal`) and span **links** become first-class. Links were the one capability [Appendix C.1](appendix-c-adversarial-review.md) confirmed `tracing` genuinely cannot express, and they are exactly what a `tokio::spawn` "this task was spawned from that one" relationship needs in Phase 2.

**Costs of this choice, stated honestly:**

- **We generate against a Beta API with no bridge absorbing breaking changes.** Mitigation is a tested `opentelemetry` version pair, a deliberately narrow generated-code surface, and the emitter seam ([R13](13-technical-risks.md)).
- **No `STATIC_MAX_LEVEL`-equivalent.** The native API has no statically-compiled-out span level. The `--cfg` gate on generated code still deletes call sites at compile time, but the "instrumented build with instrumentation runtime-disabled" configuration now costs a non-recording span rather than nothing ([R23](13-technical-risks.md), open question D-Q3).
- **No `busy`/`idle` split.** `tracing-opentelemetry` synthesises it; the OTel data model has no field for it (§5.3). A diagnostic loss, not a correctness one.
- **Our spans and a user's own `tracing` spans meet at the SDK, not in one tree.** Users already instrumenting with `tracing` keep working - #1571 resolved as "maintain both" - but nesting across the two is their existing bridge's problem, not something we provide.

**[Inference]** The emitter seam ([§11.4](11-recommended-architecture.md)) survives this reversal intact, and this round is the argument for it: a rule says "create a span here with these attributes," and a pluggable emitter decides what code that becomes. Had the seam not been the plan, this reversal would have been a rewrite instead of an emitter swap. It keeps a `tracing` emitter available for users who explicitly want their generated spans in their existing `tracing` tree, and keeps `fastrace` ([Appendix C.7](appendix-c-adversarial-review.md)) available if per-span cost is ever measured to be a problem - but the **default emitter is now the native OTel API**.

---

---

← [Existing Rust Instrumentation Landscape](04-rust-instrumentation-landscape.md) · [Contents](../../README.md) · [Rust-Specific Instrumentation Challenges](06-rust-specific-challenges.md) →
