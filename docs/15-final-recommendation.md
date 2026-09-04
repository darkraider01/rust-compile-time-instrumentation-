← [Evaluation Plan](14-evaluation-plan.md) · [Contents](../README.md) · [Appendix A — Sources](appendix-a-sources.md) →

---

## 15. Final Recommendation


### 15.1 Current conclusion

**The project is worth doing, with one significant reframing.**

What the research supports:

- **[Fact]** Rust is the conspicuous absence from OpenTelemetry's zero-code instrumentation list.
- **[Fact]** No tool exists that instruments a Rust dependency graph at build time (subject to §4.6's caveats on negative results).
- **[Fact]** Every mechanism required to build one is stable and documented: `cargo metadata`, `RUSTC_WRAPPER`, `syn`, `tracing`, `tracing-opentelemetry`, `opentelemetry-otlp`.
- **[Fact — now experimentally confirmed, not just designed; see [Appendix C](appendix-c-adversarial-review.md)]** A proven reference design exists in `otelc`, at v1.1, whose core architectural choices port to Rust with the trampoline/linkname mechanism replaced by an `extern "C"` trampoline splice — this replacement was directly tested against an undeclared, unmodified third-party dependency and works on stable Rust, surviving a deliberate adversarial challenge that claimed it could not.

What the research does **not** support:

- The framing of this as primarily a *compiler* project. The compiler-level route is nightly-pinned, permanently unstable, and — critically — makes the hardest Rust problem (async) **worse**, not better (§6.3). It is a research branch, not a product path.
- ~~Treating eBPF as a planned phase.~~ **[CLOSED — see [Appendix D.4](appendix-d-maintainer-qa.md).]** The eBPF branch is abandoned outright, and not because H2 failed. OBI maintainer Giuseppe Ognibene has a **working prototype** of Tokio async task reconstruction and context propagation in eBPF (OBI issue #1096), in final testing against task migration across worker threads and pointer reuse — the two edge cases this document independently predicted would be hardest ([Appendix C.9](appendix-c-adversarial-review.md) Q3). H2 is answered, upstream, by someone better positioned to answer it. The §15.6 pivot condition is formally executed: we build no eBPF loader and no competing implementation; we review and contribute to #1096.
- Claims of novelty for the *mechanism*. Compile-time auto-instrumentation is a solved, shipped idea in Go, and compile-time metadata embedded in a binary for eBPF consumption is a two-decade-old idea (USDT) with mature stable-Rust implementations (Appendix C.4). **The novelty is in the Rust adaptation — async semantics, monomorphization, and macro invisibility — not in the mechanism as a concept.** *(The eBPF half's narrowed novelty claim, "async-structure metadata," is withdrawn along with the branch.)*

**The honest one-line framing, updated:** *this is a port of a proven Go design to a language where nobody has done it, whose hard parts are genuinely Rust-specific, serving the platforms eBPF cannot reach.* Shorter than the previous framing by exactly the speculative research branch that has since been resolved upstream. That is a good project — arguably a better-defined one than before, since its scope no longer depends on an unvalidated hypothesis. It is not a novel-mechanism project, and describing it as one would not survive contact with someone who knows `otelc` exists.

### 15.2 Recommended architecture

**Architecture A — now the whole project, not a stage of it** ([Appendix D.4](appendix-d-maintainer-qa.md) closed Architectures C and D):

```
cargo metadata ─► rule matching ─► plan ─► pre-build runtime crate standalone
        │
        ▼
RUSTC_WRAPPER (--target-dir target/instrumented) ─► per-crate byte-range
        splice of extern "C" trampolines  __otel_span_enter / __otel_span_exit
        (syn for analysis only) ─► stock stable rustc
        │
        ▼
opentelemetry::trace  (NATIVE API — span kind, links, status)
        async sites wrapped via FutureExt::with_context
              ─► opentelemetry_sdk ─► opentelemetry-otlp ─► OTLP
```

With one structural decision taken on day one: **the emitter is a seam** (§11.4). It has already paid for itself — the reversal from `tracing` to the native OTel API ([Appendix D.2](appendix-d-maintainer-qa.md)) landed as an emitter swap rather than a rewrite.

### 15.3 Phase 1 starting point

In order:

1. **O1 is done — implement its confirmed mitigation first.** `RUSTC_WRAPPER` does *not* participate in Cargo's fingerprint (Appendix B item 2), and a `RUSTFLAGS`-based fix was shown to be destructive (Appendix C.3). The first thing to build is not an experiment but the isolated `--target-dir` mechanism itself, since everything after this step silently produces uninstrumented binaries without it.
2. **Walking skeleton.** A `RUSTC_WRAPPER` binary that logs its arguments and execs the real `rustc`, building into that isolated target dir. Confirm it is invoked for the crates we expect, including at least one dependency that does not declare our runtime crate.
3. **One hardcoded splice, across the crate boundary.** Splice an `extern "C"` trampoline call into a single named function in a dependency crate — not just the app — using `syn` for analysis and a byte-range insertion for the edit (Appendix C.2/C.6). Compile. See a span in an in-process collector, produced from inside the unmodified dependency.
4. **Async correctness before anything else.** Implement MVP success criterion 4 (total ≈ *D*, busy ≈ 0) and criterion 5 (concurrent isolation) as tests, and make them pass. Everything after this is easier; nothing after this matters if these are wrong.
5. **Then, and only then, generalise.** Rule format, matcher engine, `--plan-only`, exclusion list, snapshot tests.
6. **Then the corpus.** Run `--plan-only` over five real crates; fix what breaks.
7. **Then measure.** Build time, binary size, runtime. Publish.

**[Inference]** Steps 1 and 4 are ordered deliberately ahead of the interesting work. R1 and R5 are the two Critical risks that produce *silently wrong* behaviour, and both are cheap to test early and expensive to discover late.

### 15.4 Deferred work

| Deferred to | Item |
| --- | --- |
| **Phase 2** | Third-party dependency instrumentation (`RUSTC_WRAPPER` in full scope) |
| **Phase 2** | `wrap_call` rules; `tokio::spawn` context propagation |
| **Phase 2** | Library-specific rules (axum/hyper server spans, reqwest client spans) with semantic-convention attributes |
| **Phase 2** | Cross-process W3C context propagation |
| **Phase 2** | Third-party-distributable instrumentation crates (the `otelc` ADR-0005 analogue) |
| **Phase 2** | **[New, [Appendix D.3](appendix-d-maintainer-qa.md)]** Automated latest-version compatibility CI over instrumented crates (the `otelc` #406 pattern) — required once we ship rules for more than a handful of crates |
| ~~**Phase 3**~~ | ~~Metadata emission as a first-class build artifact (build-ID-keyed)~~ **Dropped** — superseded by USDT ([Appendix C.4](appendix-c-adversarial-review.md)), then made moot by the branch closure |
| **Phase 3 (spike)** | MIR-level instrumentation research, isolated behind a nightly-only feature flag; primarily to test the §6.3 hypothesis about pre-`StateTransform` injection |
| ~~**Phase 4 (gated)**~~ | ~~eBPF loader consuming metadata — only if H2 survives §15.6~~ **ABANDONED — [Appendix D.4](appendix-d-maintainer-qa.md).** Being built upstream (OBI #1096). Replaced by: *review Giuseppe Ognibene's upstream PR and contribute the §6.3/§7 Rust async-semantics analysis* — an ongoing collaboration, not a project phase |
| **Never (unless a user asks)** | Metrics, logs, our own exporter, our own span type, `std` instrumentation, closure instrumentation, **anything eBPF** |

### 15.5 Open questions requiring experimental validation

Ordered by how much they change the plan. **A second, non-overlapping set of open questions** — surfaced specifically by the adversarial review round (source-tree mirroring edge cases, `&Task` pointer stability, USDT's ability to carry structured async metadata, `fastrace` vs. `tracing` under our specific workload, and LTO/`panic=abort` interaction with the trampoline) — is tracked separately in [Appendix C.9](appendix-c-adversarial-review.md), so it does not get lost by being folded into this numbering.

| # | Question | Blocks | How to test |
| --- | --- | --- | --- |
| Q1 | ~~Does `RUSTC_WRAPPER` participate in Cargo's fingerprint?~~ **RESOLVED** — no, confirmed by direct experiment | Everything (R1) | **Done.** See [Appendix B](appendix-b-verification-log.md) item 2. Remaining work is implementation, not investigation: build the **isolated `--target-dir`** mechanism (**not** the `RUSTFLAGS`-hash mitigation, which [Appendix C.3](appendix-c-adversarial-review.md) showed to be destructive — it evicts the whole workspace cache and clobbers `.cargo/config.toml` rustflags) and cover it with a CI regression test that toggles the ruleset with no source change and asserts a rebuild occurred |
| Q2 | **[Reframed, [Appendix D.2](appendix-d-maintainer-qa.md)]** Can we produce correct async span semantics with generated `FutureExt::with_context` wrapping? (Was: "purely by generated `#[instrument]`") | The MVP's value | MVP criteria 4 and 5 — one span per future, duration ≈ wall clock, context absent while suspended, isolation under worker-thread migration |
| Q3 | ~~`syn`+`prettyplease` or `ra_ap_syntax`: which preserves enough fidelity to rewrite real third-party crates?~~ **RESOLVED — neither** | Phase 2 (O2, R11, R16) | **Done, [Appendix C.6](appendix-c-adversarial-review.md).** `syn`+`prettyplease`'s damage was measured (Appendix B item 7: all non-doc comments destroyed, whole-file reformat), and `cargo-mutants`' technique removes the choice entirely: `syn` for **analysis only**, then splice bytes at `span().byte_range()` into the original UTF-8 buffer. Formatting, comments, and line numbers survive byte-for-byte outside the insertion. `ra_ap_syntax` needs no evaluation. **What remains is a different question** — not "which parser," but "does source-tree *mirroring* survive real multi-file crates" ([Appendix D.6](appendix-d-maintainer-qa.md) D-Q4), which is now the project's largest open unknown |
| Q4 | What fraction of a real crate's functions do our default exclusions remove? | Whether the tool is useful at all | Instrument the corpus; report the skip-reason distribution |
| Q5 | What is the incremental build overhead? | Adoption | §14.3 |
| Q6 | Does `#[instrument]` compose with `#[async_trait]`, `#[tokio::main]`, and friends, and in what attribute order? | Real-world compatibility (O4, R6) | Compilation matrix |
| Q7 | Can a span guard be threaded through a coroutine body *before* `StateTransform` without breaking borrowck or the transform? | Whether Architecture B has any async story at all | Phase 3 nightly spike |
| Q8 | ~~**(H2)** Are logical async spans reconstructible from poll-level uprobe events plus compile-time state-machine metadata?~~ **RESOLVED UPSTREAM — CLOSED** | ~~The entire eBPF branch~~ — branch abandoned | **Done, by someone else.** [Appendix D.4](appendix-d-maintainer-qa.md): OBI maintainer Giuseppe Ognibene has a working Tokio async task reconstruction and context propagation prototype (OBI #1096), in final testing against task migration and pointer reuse. The prototype this row scheduled will not be built here |
| Q9 | ~~Does v0 mangling already supply most of what our metadata would?~~ **CLOSED — no longer load-bearing** | ~~Whether metadata is a contribution or a convenience~~ | The question existed to decide whether compiler-emitted metadata for eBPF was a research contribution. With the eBPF branch abandoned ([Appendix D.4](appendix-d-maintainer-qa.md)) we emit no such metadata, so the comparison has nothing to inform. Still a genuinely interesting question — and now one worth raising in #1096's design discussion rather than answering here |
| Q10 | ~~What is the current resolution state of opentelemetry-rust#1571?~~ **RESOLVED, and its follow-on question now also closed** | Emitter choice | **Done.** Closed 2026-03-18 as "maintain both APIs," not deprecating `tracing`. The follow-on this row raised — that `docs/traces.md` recommends the OTel API for new code, putting us in knowing divergence from upstream — **is closed by [Appendix D.2](appendix-d-maintainer-qa.md): we reversed and now generate the native OTel API.** No divergence remains to re-confirm |

**[Inference — updated]** Q8 and Q9 were framed as deciding "whether the project's research half is real." Both are now closed without our running them: Q8 answered upstream, Q9 made moot by that answer. **The project no longer has a speculative research half, and its remaining open questions are all engineering questions with known methods** — chiefly Q3/Q4 here and D-Q4 in [Appendix D.6](appendix-d-maintainer-qa.md) (does source-tree mirroring survive real multi-file crates), which is now the largest single unknown and should be attacked first.

### 15.6 Conditions for abandoning or changing course

**Abandon the compile-time approach if:**

- **[Revised twice — post-verification, then [Appendix C.3](appendix-c-adversarial-review.md)]** Q1 is resolved: `RUSTC_WRAPPER` does *not* participate in Cargo's fingerprint, and the mitigation is an **isolated `--target-dir`** (the `RUSTFLAGS`-hash alternative was shown to be destructive and is not used). The abandon condition narrows accordingly: abandon only if the isolated target dir itself fails to generalize — e.g. if it breaks under `RUSTC_WORKSPACE_WRAPPER` combined with a separate `RUSTC_WRAPPER`, or if the permanent loss of cache sharing with the user's default `target/` makes rebuild times intolerable in practice. This should surface early in Phase 1 (§15.3 step 1) if it is going to happen at all.
- Q2 shows correct async span semantics are unachievable via generated code. **[Revised, [Appendix D.2](appendix-d-maintainer-qa.md)]** This would now mean `FutureExt::with_context` — a documented, maintainer-endorsed part of the OTel Rust API — is broken, which is implausible, but it remains the load-bearing assumption.
- Q3 shows source rewriting cannot survive contact with real third-party crates. **[Re-sharpened]** The *parser* half of this is closed: byte-range splicing preserves everything outside the insertion ([Appendix C.6](appendix-c-adversarial-review.md)), so "no viable stable-Rust rewriting tool exists" is no longer a live failure mode. What could still fail is **source-tree mirroring** against real crate layouts — `include!`, `#[path]`, `build.rs`-generated modules, `sqlx::query!` ([Appendix D.6](appendix-d-maintainer-qa.md) D-Q4). If a corpus run shows most real dependencies cannot be mirrored and spliced safely, narrow the project to workspace-only instrumentation and say so plainly, rather than reconsidering Architecture B — the nightly pin remains unshippable regardless.
- Build overhead proves so large that no one would run it. **[Anchored to real data, [Appendix D.3](appendix-d-maintainer-qa.md)]** `otelc` measures +275% on a small single-package build and +54% multi-package, so 1.5×–3× clean compile is the expected range and is *not* by itself an abandon signal. The threshold that matters is incremental rebuild: consistently >2× on a one-line change is the number users will not tolerate. **Note the fallback has changed** — the old "then pivot toward the metadata/eBPF branch, which has no build cost" is gone with that branch ([Appendix D.4](appendix-d-maintainer-qa.md)). The remaining lever is scope: fewer crates instrumented by default, better splice caching, opt-in dependency coverage.

**Abandon the eBPF branch if:** — ⛔ **CONDITION TRIGGERED AND EXECUTED, 2026-09-04. The branch is abandoned.**

> **[Fact — [Appendix D.4](appendix-d-maintainer-qa.md).]** The third condition below fired, in its strongest form. OBI maintainer **Giuseppe Ognibene has a working prototype** of Tokio async task reconstruction and context propagation in eBPF, tracked under **OBI issue #1096**, in final testing against task migration across worker threads and pointer reuse. OBI maintainer Nikola Grcevski separately confirmed OBI's current Rust support remains generic socket kprobes with zero application-level uprobes — i.e. #1096 is the work that changes that, and it is happening in the canonical upstream project.
>
> **Action taken, per this section's own instruction to "contribute there instead of competing":**
> - Architectures C and D abandoned ([§10](10-architecture-candidates.md)); no eBPF loader, sidecar, or competing implementation will be built.
> - Q8/H2 closed ([§15.5](15-final-recommendation.md), [§7.5](07-ebpf-future.md)); R19–R21 closed ([§13](13-technical-risks.md)).
> - Phase 4 removed from [§15.4](15-final-recommendation.md), replaced by ongoing collaboration: review Giuseppe's upstream PR, contribute the §6.3/§7 Rust async-semantics analysis to the edge cases still in testing.
> - The project focuses 100% on Architecture A, positioned where eBPF structurally cannot go: **macOS, Windows, unprivileged containers, non-root deployments.**
>
> This is the outcome to want. The research question was real, it was answered by someone better placed to answer it, and it was answered before a phase was spent on it. Architecture A was deliberately chosen to have standalone value precisely so this closure would cost nothing structural — see the last bullet of "Do not abandon merely because," written before this happened.

The original conditions, retained as the record of what was being watched for:

- ~~Q8's first sub-question fails: no stable task identity is observable from eBPF.~~ Overtaken — the identity question was settled affirmatively in practice by a working prototype, not by our test.
- ~~Q9 shows v0 demangling already supplies the bulk of the useful metadata.~~ Moot; no metadata will be emitted.
- **OBI ships semantic Rust function-level instrumentation upstream** — **[THIS FIRED]**, via #1096. The condition anticipated a shipped release; what arrived was better: an in-progress prototype with a reachable, receptive maintainer, which is a contribution opportunity rather than a fait accompli.

**Change course toward contribution rather than construction if:**

- The OTel Rust SIG announces a compile-time instrumentation effort. **[Fact]** Rust's absence from the zero-code list is conspicuous and OTel has now shipped this for Go; someone starting it is a realistic possibility within the project's lifetime. Contributing to an official effort is a better outcome than a parallel personal tool, and the research in this document transfers directly.

**Do not abandon merely because:**

- The mechanism is not novel. It is not, and that was known from §2. Usefulness and novelty are separate axes, and the useful half is the half that is missing.
- `otelc` exists. That is evidence the design works, not evidence the Rust work is redundant.
- ~~The eBPF branch dies.~~ **It has now died, and this bullet held.** Architecture A has standalone value and was chosen specifically so that it would. The closure cost the project one deferred phase and no structural rework ([Appendix D.4](appendix-d-maintainer-qa.md)).

---

---

← [Evaluation Plan](14-evaluation-plan.md) · [Contents](../README.md) · [Appendix A — Sources](appendix-a-sources.md) →
