← [Architecture Candidates](10-architecture-candidates.md) · [Contents](../README.md) · [Phase 1 MVP Definition](12-mvp-definition.md) →

---

## 11. Recommended Architecture


### 11.1 Recommendation

**Build Architecture A. Design it so Architecture D is reachable by addition, not rewrite. Do not build B, C, or E in Phase 1.**

Concretely: a Cargo-integrated tool that

1. resolves the dependency graph (`cargo metadata`),
2. matches it against a declarative rule set,
3. intercepts compilation via `RUSTC_WRAPPER`,
4. rewrites source ASTs to inject `#[tracing::instrument]`-equivalent instrumentation,
5. hands the result to a stock stable `rustc`,
6. and ships a small runtime-init helper wiring `tracing-subscriber` → `tracing-opentelemetry` → OTLP.

### 11.2 Why this and not the more impressive options

The brief asked explicitly not to pick the most technically impressive approach. Applying the stated progression criterion:

```
small working POC  →  useful tool  →  compiler-aware  →  compiler metadata  →  optional eBPF
```

| Stage | Architecture A's path | Architecture B's path |
| --- | --- | --- |
| Small working POC | Rewrite one function in one crate, see a span in Jaeger. **Days.** | Stand up a nightly-pinned driver, get valid MIR injected, link a runtime past LTO. **Weeks, before any span exists.** |
| Useful tool | Add rules, add dependency coverage. Users can actually run it. | Users must adopt a pinned nightly. Most cannot. |
| Compiler-aware | Add a type-resolution step, or add an optional MIR-based analysis pass that *informs* source rewriting | Already there — but stuck at "unshippable" |
| Compiler metadata | The analysis phase already computes most of what metadata needs; emitting it is a serialisation step | Richer metadata available, still nightly |
| Optional eBPF | Metadata artifact already exists to feed a loader | Same |

**[Inference]** Architecture A reaches every stage of the progression. Architecture B reaches stage 1 slower and then stalls at stage 2 permanently, because the nightly pin is not a bug to be fixed — it is the permanent condition of `rustc_private` **[Fact]**.

The other decisive arguments:

- **`otelc` is an existence proof for A and not for B.** The best-resourced team to attack this problem chose AST rewriting behind a build hook and shipped v1. **[Fact]**
- **Async — the hardest Rust-specific problem — is easiest at the source layer** (§6.3). The layer that looks more powerful is the layer where our hardest problem gets worse.
- **Inspectable output.** When an auto-instrumentation tool produces a wrong span or breaks a build, the user needs to see what it did. Generated Rust can be read; injected MIR cannot.
- **Failure blast radius.** A bug in A produces a Rust compile error — annoying, obvious, recoverable. A bug in B produces a miscompiled binary or an ICE.

### 11.3 What we should explicitly NOT build initially

| Do not build | Why |
| --- | --- |
| **A custom rustc driver / MIR pass** | Nightly pin makes it unshippable; async semantics get worse, not better |
| **Anything eBPF** | Depends on H2, which is unvalidated; different skill domain; would consume the whole project |
| **An LLVM pass** | Dominated by both A and B; duplicates `-Z instrument-xray` |
| **A new telemetry abstraction / our own span type** | `tracing` exists, has 818M downloads, and solved async |
| **Our own OTLP exporter** | `opentelemetry-otlp` exists |
| **A `tracing-opentelemetry` replacement** | It is maintained, mature, and absorbs OTel API churn for us |
| **Metrics or logs** | Traces only in Phase 1. Metrics is `autometrics`'s territory and a separate problem |
| **`std` instrumentation** | Requires `-Z build-std` (nightly) |
| **Distributed context propagation** | Requires library-specific rules; Phase 2 at the earliest |
| **Argument/return value capture by default** | Security risk (PII/secret exfiltration) and performance risk |
| **Support for every Rust construct** | §6.12 exclusion list is a feature |
| **A GUI, a dashboard, a backend, a collector** | The OTel ecosystem has these |

### 11.4 The one non-obvious design decision to make on day one

**[Inference]** Make the *emitter* a seam. A rule should say "create a span here, named X, with attributes Y"; a pluggable emitter decides whether that becomes a `#[tracing::instrument]` attribute, an explicit `tracing::span!` + guard, a raw OTel API call, or a metadata record with no code emitted at all.

This costs perhaps a day now. It buys:
- insurance against #1571 resolving against `tracing` (§5.2),
- the metadata-emission path for free (a "metadata-only" emitter is the Architecture C/D bridge),
- testability — a "record what you would have done" emitter makes rule-matching unit-testable without compiling anything.

This is the only piece of speculative generality worth paying for.

---

---

← [Architecture Candidates](10-architecture-candidates.md) · [Contents](../README.md) · [Phase 1 MVP Definition](12-mvp-definition.md) →
