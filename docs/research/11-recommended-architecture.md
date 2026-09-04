← [Architecture Candidates](10-architecture-candidates.md) · [Contents](../../README.md) · [Phase 1 MVP Definition](12-mvp-definition.md) →

---

## 11. Recommended Architecture


### 11.1 Recommendation

**Build Architecture A. Do not build B, C, D, or E - in Phase 1 or after.** *(Previously: "design it so Architecture D is reachable by addition." D is now closed - [Appendix D.4](appendix-d-maintainer-qa.md) - so A is not a stepping stone to anything; it is the deliverable.)*

**[Revised after the adversarial review round - see [Appendix C](appendix-c-adversarial-review.md).** An independent review challenged whether Architecture A can instrument third-party dependencies on stable Rust at all, calling it a "fatal structural flaw." It was tested directly and works, on stable, with no `-Zunstable-options` - but the mechanism below has been corrected from the original write-up in two ways the review got right: trampolines instead of injected crate dependencies, and byte-range splicing instead of `syn`→`prettyplease`.]**

**[Revised again after the maintainer Q&A round - see [Appendix D](appendix-d-maintainer-qa.md).** Two changes. The emitted telemetry is now the **native OpenTelemetry API**, not `tracing` (D.2). And **Architecture D is no longer the growth path** (D.4): the eBPF half it was reaching toward is being built upstream in OBI (#1096), so "design so D is reachable by addition" is retired. What replaces it is not a smaller ambition but a clearer one - Architecture A alone, serving the platforms eBPF cannot reach.]**

Concretely: a Cargo-integrated tool that

1. resolves the dependency graph (`cargo metadata`) and pre-builds the runtime crate standalone, outside Cargo's own dependency graph,
2. matches the graph against a declarative rule set,
3. intercepts compilation via `RUSTC_WRAPPER`, building into an isolated `--target-dir` rather than mutating `RUSTFLAGS`,
4. splices `extern "C"` trampoline calls (`__otel_span_enter` / `__otel_span_exit`) into the original source buffer by byte offset (no `--extern`, no crate-graph edge - see [Appendix C.2](appendix-c-adversarial-review.md)),
5. hands the result to a stock stable `rustc`,
6. and ships a small runtime-init helper wiring `opentelemetry_sdk` → `opentelemetry-otlp` directly, with generated async sites wrapped via `opentelemetry::trace::FutureExt::with_context` and the trampoline symbols resolved at the application's own final link step.

### 11.2 Why this and not the more impressive options

The brief asked explicitly not to pick the most technically impressive approach. Applying the stated progression criterion:

```
small working POC  →  useful tool  →  compiler-aware  →  compiler metadata  →  optional eBPF
```

| Stage | Architecture A's path | Architecture B's path |
| --- | --- | --- |
| Small working POC | Rewrite one function in one crate, see a span in Jaeger. **Days.** | Stand up a nightly-pinned driver, get valid MIR injected, link a runtime past LTO. **Weeks, before any span exists.** |
| Useful tool | Add rules, add dependency coverage. Users can actually run it. | Users must adopt a pinned nightly. Most cannot. |
| Compiler-aware | Add a type-resolution step, or add an optional MIR-based analysis pass that *informs* source rewriting | Already there - but stuck at "unshippable" |
| Compiler metadata | ~~The analysis phase already computes most of what metadata needs; emitting it is a serialisation step~~ **Retired** - [Appendix D.4](appendix-d-maintainer-qa.md) | Retired for the same reason |
| Optional eBPF | ~~Metadata artifact already exists to feed a loader~~ **Retired.** Being built upstream in OBI #1096; the progression now ends at "useful tool" and grows sideways into more rules and more platforms, not downward into the kernel | Same |

**[Inference]** Architecture A reaches every stage of the progression. Architecture B reaches stage 1 slower and then stalls at stage 2 permanently, because the nightly pin is not a bug to be fixed - it is the permanent condition of `rustc_private` **[Fact]**.

The other decisive arguments:

- **`otelc` is an existence proof for A and not for B.** The best-resourced team to attack this problem chose AST rewriting behind a build hook and shipped v1. **[Fact]**
- **Async - the hardest Rust-specific problem - is easiest at the source layer** (§6.3). The layer that looks more powerful is the layer where our hardest problem gets worse.
- **Inspectable output.** When an auto-instrumentation tool produces a wrong span or breaks a build, the user needs to see what it did. Generated Rust can be read; injected MIR cannot.
- **Failure blast radius.** A bug in A produces a Rust compile error - annoying, obvious, recoverable. A bug in B produces a miscompiled binary or an ICE.

### 11.3 What we should explicitly NOT build initially

| Do not build | Why |
| --- | --- |
| **A custom rustc driver / MIR pass** | Nightly pin makes it unshippable; async semantics get worse, not better |
| **Anything eBPF** | **[Hardened, [Appendix D.4](appendix-d-maintainer-qa.md)]** No longer "deferred pending H2" but permanently out of scope: OBI #1096 is building it upstream. Contribute there; do not compete |
| **An LLVM pass** | Dominated by both A and B; duplicates `-Z instrument-xray` |
| **A new telemetry abstraction / our own span type** | The OpenTelemetry Rust API exists, handles async correctly via `FutureExt::with_context`, and is what upstream tells new code to use ([Appendix D.2](appendix-d-maintainer-qa.md)). (`tracing` and `fastrace` remain available behind the §11.4 emitter seam - for users who want generated spans in their existing `tracing` tree, or if per-span cost is ever measured to be a problem - but neither is the default.) |
| **Our own OTLP exporter** | `opentelemetry-otlp` exists |
| **A `tracing-opentelemetry` replacement** | **[Revised, [Appendix D.2](appendix-d-maintainer-qa.md)]** Still true, and now irrelevant: we no longer depend on it at all. Its maintainer calls its context-sync bridge *"super hairy / hot-path-y / full of terror"* - a component to neither replace nor take a hot-path dependency on |
| **A bespoke metadata sidecar file for eBPF** | USDT already solves "compile-time probe metadata embedded in the binary" (Appendix C.4) - and with the eBPF branch closed we emit no eBPF metadata in any format |
| **Metrics or logs** | Traces only in Phase 1. Metrics is `autometrics`'s territory and a separate problem |
| **`std` instrumentation** | Requires `-Z build-std` (nightly) |
| **Distributed context propagation** | Requires library-specific rules; Phase 2 at the earliest |
| **Argument/return value capture by default** | Security risk (PII/secret exfiltration) and performance risk |
| **Support for every Rust construct** | §6.12 exclusion list is a feature |
| **A GUI, a dashboard, a backend, a collector** | The OTel ecosystem has these |

### 11.4 The one non-obvious design decision to make on day one

**[Inference - and the maintainer Q&A round vindicated it; see [Appendix D.2](appendix-d-maintainer-qa.md).]** Make the *emitter* a seam. A rule should say "create a span here, named X, with attributes Y"; a pluggable emitter decides whether that becomes a native OTel API call, a `#[tracing::instrument]` attribute, or a record with no code emitted at all.

**The default emitter is the native OpenTelemetry API** - `tracer.start(...)` with span kind and attributes, and `FutureExt::with_context` wrapping for async sites.

This costs perhaps a day. What it has already bought, and still buys:

- **It absorbed a full reversal of the emitter decision at zero structural cost.** [Appendix D.2](appendix-d-maintainer-qa.md) overturned `tracing` in favour of the native API after this document had committed to `tracing` throughout. Because the seam was the plan, that is an emitter swap and a documentation pass, not a rewrite. This was the exact class of event the seam was speculative insurance against; it happened within one review round.
- A `tracing` emitter stays available for users who want generated spans inside their existing `tracing` tree, and `fastrace` ([Appendix C.7](appendix-c-adversarial-review.md)) stays available if per-span cost is measured to be a problem.
- Testability - a "record what you would have done" emitter makes rule-matching unit-testable without compiling anything.

~~The metadata-emission path for free (a "metadata-only" emitter is the Architecture C/D bridge).~~ **Retired** - Architectures C and D are closed ([Appendix D.4](appendix-d-maintainer-qa.md)). The dry-run emitter that backs `--plan-only` covers what remains useful about that idea.

This is the only piece of speculative generality worth paying for.

---

---

← [Architecture Candidates](10-architecture-candidates.md) · [Contents](../../README.md) · [Phase 1 MVP Definition](12-mvp-definition.md) →
