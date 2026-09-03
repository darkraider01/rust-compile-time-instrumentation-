← [Existing Rust Instrumentation Landscape](04-rust-instrumentation-landscape.md) · [Contents](README.md) · [Rust-Specific Instrumentation Challenges](06-rust-specific-challenges.md) →

---

## 5. OpenTelemetry Rust — Current Architecture and What We Should Target


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

**[Inference — important and uncomfortable]** This is the inverse of Go, Java, and most other OTel implementations, where traces stabilised first. **The exact signal our tool would generate — spans — is the one part of OpenTelemetry Rust that is still Beta.** Practical consequences:

1. We must expect breaking changes in the traces API/SDK across `opentelemetry` minor versions during the project's life.
2. Any code we *generate* should depend on the traces API as indirectly as possible, so that an `opentelemetry` bump does not require regenerating or reshipping instrumentation.
3. This is a further argument for generating `tracing` calls: `tracing` 0.1.x has been API-stable for years, and the churn is absorbed by `tracing-opentelemetry` — a crate someone else maintains.

### 5.2 The two-API problem — RESOLVED since the original research pass

**[Fact]** [open-telemetry/opentelemetry-rust#1571](https://github.com/open-telemetry/opentelemetry-rust/issues/1571) — "OpenTelemetry Tracing API vs Tokio-Tracing API for Distributed Tracing" — framed the problem as users being forced to choose between two competing span APIs:

- the `opentelemetry` crate's Tracing API, aligned with the cross-language OTel specification;
- the `tracing` crate, which predates OTel in Rust, has overwhelming ecosystem adoption, and is actively maintained.

**[Fact]** Problems cited: incomplete traces when different layers of an application use different APIs; log↔trace correlation breaking; and no comprehensively tested interoperability between them. `tracing-opentelemetry` was described as a mitigation, not a solution. Four options were enumerated — deprecate tokio-tracing (judged improbable given adoption); deprecate the overlapping OTel APIs and treat `tracing` as the Rust standard; maintain both with seamless interoperability; or stop accommodating each other.

**[Fact — resolved in the verification pass]** The issue is **closed** (`state_reason: completed`, closed 2026-03-18, 47 comments). The maintainers settled on **"Option 3 — Maintain Both APIs"** (per maintainer scottgerring, 2025-04-01): both the OTel-native API and `tracing` remain first-class, with the goal of fixing interoperability (the "context synchronisation issue" tracked separately in [#1690](https://github.com/open-telemetry/opentelemetry-rust/issues/1690)) rather than deprecating either. The closing comment (2026-03-18) states plainly: "Interop works now; unified context would be nice but seems unrealistic. Let's put a line under it." So `tracing` is **not** at risk of being deprecated in favour of the OTel API — both are permanent, sanctioned choices.

**[Fact — new finding, and this is the one that actually matters for §5.4]** The PR that closed out the issue's action items, [#3122](https://github.com/open-telemetry/opentelemetry-rust/pull/3122) ("docs: start doc for distributed tracing and logs guidance", merged), added `docs/traces.md` to the opentelemetry-rust repository. That document — the project's own current, official guidance, published *after* the two-API debate concluded — states:

> "For new code, prefer the OpenTelemetry Tracing API directly."

citing `tracing`'s lack of span kind, links, and remote-parent support as the reason. `docs/traces.md` is itself marked **Work-In-Progress** (traces remain Beta), while the companion `docs/logs.md` is marked Stable.

**[Inference — a real tension, not swept aside]** So the picture is more complicated than "both APIs survive, therefore our choice is safe." Both APIs *do* survive, but the project maintainers' own current advice for *new* code is the opposite of what we recommend in §5.4. That recommendation is not wrong on its technical merits (§5.4 restates and re-weighs it below), but it is now a **considered disagreement with upstream guidance**, not merely "whichever API the ecosystem happens to use," and should be presented to stakeholders as such.

### 5.3 Span lifecycle, context propagation, semantic conventions

**Span lifecycle.** In `tracing`, a span is created, then *entered* and *exited* possibly many times, and finally closed when the last handle is dropped. Entering/exiting is not the same as starting/ending. `tracing-opentelemetry` maps a `tracing` span's full lifetime (creation → close) onto an OTel span's start → end; the multiple enter/exit pairs of an async span become `busy`/`idle` timing rather than separate spans.

**[Inference]** This mapping is *the* reason `tracing` is the right target for async Rust. An OTel span has one start and one end. An async Rust function's execution is interleaved: it is polled, suspends, is polled again on possibly a different thread. Only a model with a separate "entered" concept can represent that faithfully, and `tracing` + `tracing-opentelemetry` already implement it. Rebuilding this on the raw OTel API would mean rebuilding `tracing-opentelemetry`.

**Context propagation.** OTel Rust provides `Context`, `TextMapPropagator`, and a W3C TraceContext propagator; injection/extraction at process boundaries is manual — you call `propagator.inject_context(...)` on outbound requests and `extract(...)` on inbound ones. **[Fact]** `tracing-opentelemetry`'s `OpenTelemetrySpanExt` exposes `set_parent`/`context` so a remote parent can be attached to a `tracing` span.

**[Inference]** Cross-process propagation is where automatic instrumentation earns most of its value and where it is hardest: it requires knowing the *type* of the outbound request object (a `reqwest::RequestBuilder`, a `tonic` request, a `hyper::Request`) in order to inject headers. This is a *library-specific* rule, not a generic function rule — which is precisely why `otelc` shipped `net/http` and gRPC rules rather than a generic mechanism, and why our Phase 1 must not promise distributed propagation.

**Semantic conventions.** The `opentelemetry-semantic-conventions` crate provides generated constants (`HTTP_REQUEST_METHOD`, `SERVER_ADDRESS`, `DB_SYSTEM_NAME`, …). **[Inference]** Generic function instrumentation cannot produce semantic-convention-compliant attributes — there is no convention for "function `foo::bar` was called." Semantic conventions only apply to *library-specific* rules. Phase 1 should therefore emit `code.*`-style attributes (function name, module path, file, line) and explicitly not claim semantic-convention compliance.

**[Fact — resolved in the verification pass]** The `code.*` attribute group was promoted to **Stable** in semantic conventions v1.33.0. The stable, current attribute names (renamed from earlier experimental names) are: `code.function.name` (fully-qualified name, without arguments — replaces the old `code.namespace` + function-name split), `code.file.path` (replaces `code.filepath`), `code.line.number` (replaces `code.lineno`), `code.column.number`, and `code.stacktrace`. These are the attribute names Phase 1 should emit; see [opentelemetry.io/docs/specs/semconv/registry/attributes/code/](https://opentelemetry.io/docs/specs/semconv/registry/attributes/code/) and the [migration guide](https://opentelemetry.io/docs/specs/semconv/non-normative/code-attrs-migration/) for instrumentation authors migrating from the old names.

### 5.4 Decision: what should the tool generate?

> **Should compiler instrumentation generate `tracing` instrumentation, directly use OpenTelemetry APIs, or use another abstraction?**

**Recommendation, held with revised (medium, not high) confidence after verification: generate `tracing` instrumentation — specifically `#[tracing::instrument]` where possible, and explicit `tracing::span!` + guard where not. Do not generate raw OTel API calls. Do not invent a third abstraction.**

**This recommendation now knowingly goes against the OpenTelemetry Rust project's own current published guidance** ("For new code, prefer the OpenTelemetry Tracing API directly," `docs/traces.md`, §5.2). That guidance is aimed at humans writing manual instrumentation, where OTel's richer model (span kind, links, remote-parent support) is worth the API's Beta-status risk. Our situation is different in a way that we believe still justifies the divergence — argued below — but this is a real trade-off being made against upstream advice, not an uncontested default, and should be revisited if opentelemetry-rust's traces API stabilises before our Phase 1 implementation begins.

Reasoning:

1. **Async semantics are already solved there and nowhere else.** `#[instrument]` on an `async fn` produces an `Instrumented` future with correct enter/exit-per-poll behaviour **[Fact]**. Generating raw OTel spans in an async function would produce a span whose "duration" includes every period the task was suspended, silently and wrongly. Getting this right ourselves means reimplementing `tracing-opentelemetry`.
2. **The traces API we would target directly is Beta; `tracing` 0.1 is not.** **[Fact]** Generating against the less stable of two available APIs is the wrong risk trade.
3. **Ecosystem convergence.** Libraries that *do* instrument themselves (`tokio`, `hyper`, `axum`, `sqlx`, `tower-http`) emit `tracing` spans. If we generate `tracing`, our spans nest correctly inside and around theirs for free. If we generate raw OTel spans, we create two parallel trees that `tracing-opentelemetry` must reconcile.
4. **Free compile-time kill switch.** `STATIC_MAX_LEVEL` lets users compile our instrumentation out entirely with a Cargo feature **[Fact]**, with no support burden on us.
5. **Debuggability.** Generated `#[instrument]` attributes are readable Rust that a developer can inspect, `cargo expand`, and reason about. Generated raw span plumbing is not.
6. **Someone else maintains the hard part.** `tracing-opentelemetry` absorbs OTel API churn, `Context` mapping, and sampling interaction. That is a large, ongoing maintenance load we get for free.

**Costs of this choice, stated honestly — updated post-verification:**

- We inherit `tracing`'s model, including its `busy`/`idle` semantics, which are not what an OTel-native user necessarily expects.
- We add a dependency on a bridge crate whose version numbering is deliberately offset from `opentelemetry`'s **[Fact]**, which is an ongoing dependency-resolution hazard for a tool that injects both.
- **We are explicitly not following the project's own current recommendation for new code** (§5.2). Reasons 2 and 6 below (Beta traces API, `tracing-opentelemetry` absorbing churn) are arguments about *our* risk as tool authors generating code automatically at scale; `docs/traces.md`'s reasoning (span kind, links, remote-parent support) is about expressiveness for a human writing one span by hand. We believe the risk argument dominates for an auto-instrumentation tool specifically — but this is a judgment call under genuine disagreement with the upstream project, not a settled question, and it should be re-examined once traces stabilise. **#1571 itself no longer poses this risk** — it resolved to "maintain both," not to deprecating `tracing` — so the residual risk is narrower than originally scoped: not "our choice becomes unsupported," but "our choice becomes non-idiomatic by the project's own stated preference."

**[Inference]** The right structural hedge is to make the *emitted telemetry backend* a rule-level choice from the start — i.e. the rule says "create a span here with these attributes," and a pluggable emitter decides whether that becomes `#[instrument]`, a `tracing::span!`, or an OTel call. That costs almost nothing to design in on day one and costs a rewrite to retrofit. It is *not* a third abstraction for users; it is one internal seam.

---

---

← [Existing Rust Instrumentation Landscape](04-rust-instrumentation-landscape.md) · [Contents](README.md) · [Rust-Specific Instrumentation Challenges](06-rust-specific-challenges.md) →
