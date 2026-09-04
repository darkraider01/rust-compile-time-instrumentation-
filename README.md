# Phase 0 — Landscape Research & Architecture

**Investigation date:** 2026-09-04 (research pass, verification pass, an adversarial review round, and a maintainer Q&A round across three OTel SIGs — see [Appendix B](docs/appendix-b-verification-log.md), [Appendix C](docs/appendix-c-adversarial-review.md), and [Appendix D](docs/appendix-d-maintainer-qa.md))
**Status:** Research artifact. Nothing here is a binding implementation decision until [§15](docs/15-final-recommendation.md) is reviewed.
**Toolchain baseline:** Rust 1.98.0 stable (2026-08-20); `opentelemetry` 0.32.x; `otelc` v1.1.0. All hands-on experiments (Appendix B and Appendix C) were run against the locally installed Rust 1.97.1 / Cargo 1.97.1 toolchain. *(`tracing` 0.1.44 / `tracing-opentelemetry` 0.33.0 were baseline dependencies until [Appendix D.2](docs/appendix-d-maintainer-qa.md) removed them from the design.)*

> **Two decisions were reversed by the maintainer Q&A round ([Appendix D](docs/appendix-d-maintainer-qa.md)). Read that before acting on anything below.**
>
> 1. **Code generation targets the native OpenTelemetry API, not `tracing`.** OTel Rust maintainer Scott Gerring disproved this document's central premise that only `tracing` handles async future interleaving correctly — `opentelemetry::trace::FutureExt::with_context` attaches the context on each `poll()` and detaches on yield, matching `tracing::Instrument`. `tracing`, `tracing-subscriber`, and `tracing-opentelemetry` leave the injected dependency set.
> 2. **The eBPF branch is closed.** OBI maintainer Giuseppe Ognibene has a working prototype of Tokio async task reconstruction in eBPF (OBI #1096), resolving hypothesis H2 upstream. The [§15.6](docs/15-final-recommendation.md) pivot condition has fired: Architectures C and D are abandoned, and the project is now 100% compile-time Cargo instrumentation, serving the platforms eBPF cannot reach.

This research is split into one file per section so each can be read, linked, and updated independently. Start here, then follow the table of contents.

### Evidence labelling convention

| Tag | Meaning |
| --- | --- |
| **[Fact]** | Directly verifiable from a cited primary source (docs, source code, repository metadata) — or, where noted, from a hands-on experiment run during this investigation. |
| **[Inference]** | A conclusion drawn from one or more Facts. The reasoning is stated and could be wrong. |
| **[Hypothesis]** | A plausible claim believed but **not** verified. Must be experimentally validated before it drives a design decision. |
| **[Open question]** | Something not known and not determinable from available sources. |

Anything untagged is background or editorial framing, not a load-bearing technical claim.

### Table of contents

1. **Executive Summary** — this file, below
2. [OpenTelemetry Go Compile-Time Instrumentation (`otelc`)](docs/02-otelc-go.md)
3. [The Rust Compilation Pipeline](docs/03-rust-compiler-pipeline.md)
4. [Existing Rust Instrumentation Landscape](docs/04-rust-instrumentation-landscape.md)
5. [OpenTelemetry Rust — Current Architecture and What We Should Target](docs/05-otel-rust.md)
6. [Rust-Specific Instrumentation Challenges](docs/06-rust-specific-challenges.md)
7. [eBPF as a Future Extension](docs/07-ebpf-future.md)
8. [Competitive / Adjacent Landscape](docs/08-competitive-landscape.md)
9. [Gap Analysis](docs/09-gap-analysis.md)
10. [Architecture Candidates](docs/10-architecture-candidates.md)
11. [Recommended Architecture](docs/11-recommended-architecture.md)
12. [Phase 1 MVP Definition](docs/12-mvp-definition.md)
13. [Technical Risks](docs/13-technical-risks.md)
14. [Evaluation Plan](docs/14-evaluation-plan.md)
15. [Final Recommendation](docs/15-final-recommendation.md)
- [Appendix A — Primary sources consulted](docs/appendix-a-sources.md)
- [Appendix B — Verification log](docs/appendix-b-verification-log.md) *(supersedes the original "claims not verified" list — every item there was subsequently checked, several by hands-on experiment)*
- [Appendix C — Adversarial review round](docs/appendix-c-adversarial-review.md) *(an independent review challenged the core architecture; settled by experiment — most of Architecture A survived, several mechanism details did not)*
- [Appendix D — Maintainer Q&A round](docs/appendix-d-maintainer-qa.md) *(direct answers from maintainers in `#otel-rust`, `#otel-go`, and `#otel-ebpf` — reversed the emitter decision and closed the eBPF branch)*

---

## 1. Executive Summary

### 1.1 The problem

Rust has no zero-code observability story. Every other major server-side language on OpenTelemetry's zero-code page — Java, .NET, Node.js, Python, PHP, Go — has an official mechanism to obtain traces without editing application source. **[Fact]** Rust is not listed on [opentelemetry.io/docs/zero-code/](https://opentelemetry.io/docs/zero-code/).

In practice, instrumenting a Rust service today means:

1. Add `tracing`, `tracing-subscriber`, `opentelemetry`, `opentelemetry-otlp`, `tracing-opentelemetry`.
2. Hand-write a subscriber/exporter pipeline in `main`.
3. Manually sprinkle `#[tracing::instrument]` on the functions you care about.
4. Get zero visibility into third-party crates that did not instrument themselves.

Step 4 is the structural problem. In Go, `otelc` instruments `net/http`, `database/sql`, and gRPC *inside your dependency tree*, because it rewrites those packages during the build. In Rust, if `sqlx` or `hyper` did not emit `tracing` spans, you get nothing, and you cannot fix it without vendoring or patching the dependency.

### 1.2 What already solves parts of it

| Concern | Existing solution | Gap it leaves |
| --- | --- | --- |
| Per-function span creation | `#[tracing::instrument]` | Manual, per-function, first-party code only |
| tracing → OTel bridge | `tracing-opentelemetry` | Mature — but you still have to produce the tracing spans |
| OTLP export | `opentelemetry-otlp` | Solved |
| Zero-code, no rebuild | OBI (eBPF, ex-Beyla) | For Rust *today*: syscall/socket-boundary telemetry only — **[Fact, confirmed in verification and again by OBI maintainer Nikola Grcevski]** OBI attaches no function-level uprobes into Rust application code (§7, Appendix B). **[New — [Appendix D.4](docs/appendix-d-maintainer-qa.md)]** A working prototype for Tokio async reconstruction exists upstream (OBI #1096). The durable gap is not capability but **reach**: eBPF needs Linux and privileges, so macOS, Windows, unprivileged containers, and non-root deployments stay uncovered by any version of this |
| Compile-time injection into dependencies | **nothing in Rust** | This is the gap |

### 1.3 Does a gap exist?

**Yes, but it is narrower than the project hypothesis assumes.** **[Inference]**

- The *export/SDK* layer is solved and must not be rebuilt.
- The *span-producing macro* layer is solved for code you own (`#[instrument]`).
- The *automatic, whole-dependency-graph, build-time injection* layer does not exist for Rust. We searched crates.io and the web and found no tool that instruments an entire Rust crate graph for observability. **[Fact — negative result; see §4.6 for search scope and its limits]**
- The *eBPF* layer exists (OBI) but is confirmed shallow for Rust today — kernel-syscall and socket-filter telemetry only, not application-function spans (§7). **[Updated — [Appendix D.4](docs/appendix-d-maintainer-qa.md)]** This is actively being fixed upstream: an OBI maintainer has a working Tokio async reconstruction prototype (#1096). **We are not building in this space** — the eBPF branch is closed, and the compile-time tool's durable niche is the platforms eBPF cannot run on at all.

### 1.4 Most promising technical direction

**A Cargo-integrated `RUSTC_WRAPPER` tool that splices `extern "C"` trampoline instrumentation into source at the byte level before handing code to a stock `rustc`, emitting native OpenTelemetry API calls — with async sites wrapped via `opentelemetry::trace::FutureExt::with_context`.**

**[Revised after an adversarial review round — see [Appendix C](docs/appendix-c-adversarial-review.md).** An independent review argued this architecture is "structurally blocked on stable Rust" and cannot instrument third-party dependencies at all. That was tested directly by building and running the mechanism: it works, on stable Rust, with no `-Zunstable-options`. Two of the review's specific mechanism criticisms were nonetheless correct and are folded in here: extern `"C"` trampolines (not injecting a crate dependency) avoid all crate-graph/cycle hazards, and byte-range source splicing (not `syn`→`prettyplease`) avoids destroying comments and formatting.]**

**[Revised again after the maintainer Q&A round — see [Appendix D.2](docs/appendix-d-maintainer-qa.md).** The emitted telemetry changed from `tracing` spans to native OTel API calls. The sole technical argument for `tracing` was async lifecycle handling, and an OTel Rust maintainer disproved it: *"If your goal is compile-time instrumentation, I don't see async future handling as a reason to favour the tracing api."* The reversal drops three injected dependencies, aligns with upstream's own `docs/traces.md` guidance instead of diverging from it, and restores first-class span kinds and links. The injection mechanism is unchanged.]**

This is deliberately the *least* impressive of the candidate architectures. The reasoning (§9, §11):

- It is the direct structural analogue of `otelc`'s `-toolexec` design — the one design in this space that reached v1.0 and production use. **[Fact]**
- It runs on **stable** Rust, so it is shippable and maintainable.
- It reaches third-party dependencies — the actual gap — because `RUSTC_WRAPPER` sees every crate in the graph, not just workspace members, and this was confirmed by direct experiment against an undeclared, unmodified dependency, not merely assumed. **[Fact]**
- It produces a working POC in weeks, and it is a stepping stone to the compiler-level and eBPF work rather than a dead end.

The MIR / rustc-driver approach (Architecture B) is more technically interesting and is where the genuinely novel research lies, but it is nightly-pinned, effectively unshippable to real users, and — critically — **async semantics get harder, not easier, at MIR level** (§6.3). It belongs in Phase 3+ as a research branch, not in Phase 1.

### 1.5 Confidence

| Claim | Confidence | Why |
| --- | --- | --- |
| No existing Rust whole-graph compile-time OTel instrumentation tool | **Medium-high** | Negative search results are inherently weak, but we checked crates.io, OTel's official zero-code list, and the OTel Rust SIG surface |
| `RUSTC_WRAPPER` source instrumentation can reach third-party dependencies on stable Rust | **Very high — confirmed by direct experiment**, not merely asserted | An adversarial review called this "structurally impossible." Directly tested: an undeclared, unmodified dependency was successfully instrumented via an `extern "C"` trampoline splice, with no `-Zunstable-options`. See Appendix C.2 |
| Cargo's build cache will silently serve an uninstrumented artifact unless the tool actively busts it | **Very high — confirmed by direct experiment**, not inferred | Verification pass reproduced this on Cargo 1.97.1/Windows: toggling `RUSTC_WRAPPER` on an already-built crate triggers zero recompilation. The original proposed fix (`RUSTFLAGS`-hashing) was itself shown to be destructive — it evicts the whole workspace cache and clobbers user config — and was replaced with an isolated `--target-dir`, also confirmed by experiment. See Appendix B item 2 and Appendix C.3 |
| MIR instrumentation is feasible but nightly-only and version-fragile | **High** | Directly evidenced by rustc source and `rustc_plugin`'s per-nightly pinning |
| Source-level rewriting via `syn`→`prettyplease` silently drops all non-doc comments and reformats the whole file | **Very high — confirmed by direct experiment**, and now moot | A hands-on round-trip test destroyed 4/4 line comments while preserving all doc comments and structural content (Appendix B item 7). The design has since moved to byte-range splicing (following `cargo-mutants`' proven technique) specifically to avoid this — see Appendix C.6 |
| ~~Compiler-generated metadata would materially improve eBPF instrumentation for Rust~~ **WITHDRAWN — branch closed** | — | The mechanism was already shown not to be novel (USDT, Appendix C.4), narrowing the claim to *async state-machine structure*. **[Appendix D.4](docs/appendix-d-maintainer-qa.md)** then resolved that too: an OBI maintainer has a working Tokio async reconstruction prototype (#1096). We emit no eBPF metadata and build no loader |
| ~~Generating `tracing` output (vs. the OTel API directly) is the right MVP choice~~ **REVERSED** | **High confidence in the reversal** | The claim rested on `tracing` being the only correct model for async future interleaving. **[Appendix D.2](docs/appendix-d-maintainer-qa.md)** — OTel Rust maintainer Scott Gerring, plus `opentelemetry::trace::FutureExt::with_context` as primary evidence: the native API attaches per-`poll()` and detaches on yield, exactly as `tracing::Instrument` does. **We now generate the native OTel API**, matching upstream's own guidance. Costs accepted: direct exposure to the Beta traces API ([R13](docs/13-technical-risks.md)) and no `STATIC_MAX_LEVEL` analogue ([R23](docs/13-technical-risks.md)) |
| Architecture A is worth building on its own, with no eBPF branch attached | **High — and no longer conditional** | Previously "low-medium" for the project *as a whole*, because it bundled a proven compile-time half with an unvalidated eBPF half. The eBPF half has been resolved upstream and removed ([Appendix D.4](docs/appendix-d-maintainer-qa.md)), leaving a project whose mechanism is experimentally confirmed (Appendix C.2), whose emitter now matches upstream guidance (Appendix D.2), and whose niche — platforms eBPF cannot reach — is structural rather than a race |

---

*Continue to [§2 — OpenTelemetry Go Compile-Time Instrumentation](docs/02-otelc-go.md).*
