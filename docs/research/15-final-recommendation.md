← [Evaluation Plan](14-evaluation-plan.md) · [Contents](../../README.md) · [Instrumentation Semantics](16-instrumentation-semantics.md) →

---

## 15. Final Recommendation


### 15.1 Current conclusion

**The project is worth doing, with one significant reframing.**

What the research supports:

- **[Fact]** Rust is the conspicuous absence from OpenTelemetry's zero-code instrumentation list.
- **[Fact]** No tool exists that instruments a Rust dependency graph at build time (subject to §4.6's caveats on negative results).
- **[Fact - updated, [Appendix D.2](appendix-d-maintainer-qa.md)]** Every mechanism required to build one is stable and documented: `cargo metadata`, `RUSTC_WRAPPER`, `syn` (analysis + `span().byte_range()`), `opentelemetry` (including `trace::FutureExt`), `opentelemetry_sdk`, `opentelemetry-otlp`. *(`tracing` and `tracing-opentelemetry` were on this list until [ADR-001](17-decision-records.md) removed them from the design.)*
- **[Fact - now experimentally confirmed, not just designed; see [Appendix C](appendix-c-adversarial-review.md)]** A proven reference design exists in `otelc`, at v1.1, whose core architectural choices port to Rust with the trampoline/linkname mechanism replaced by an `extern "C"` trampoline splice - this replacement was directly tested against an undeclared, unmodified third-party dependency and works on stable Rust, surviving a deliberate adversarial challenge that claimed it could not.

What the research does **not** support:

- The framing of this as primarily a *compiler* project. The compiler-level route is nightly-pinned, permanently unstable, and - critically - makes the hardest Rust problem (async) **worse**, not better (§6.3). It is a research branch, not a product path.
- ~~Treating eBPF as a planned phase.~~ **[CLOSED - see [Appendix D.4](appendix-d-maintainer-qa.md).]** The eBPF branch is abandoned outright, and not because H2 failed. OBI maintainer Giuseppe Ognibene has a **working prototype** of Tokio async task reconstruction and context propagation in eBPF (OBI issue #1096), in final testing against task migration across worker threads and pointer reuse - the two edge cases this document independently predicted would be hardest ([Appendix C.9](appendix-c-adversarial-review.md) Q3). H2 is answered, upstream, by someone better positioned to answer it. The §15.6 pivot condition is formally executed: we build no eBPF loader and no competing implementation; we review and contribute to #1096.
- Claims of novelty for the *mechanism*. Compile-time auto-instrumentation is a solved, shipped idea in Go, and compile-time metadata embedded in a binary for eBPF consumption is a two-decade-old idea (USDT) with mature stable-Rust implementations (Appendix C.4). **The novelty is in the Rust adaptation - async semantics, monomorphization, and macro invisibility - not in the mechanism as a concept.** *(The eBPF half's narrowed novelty claim, "async-structure metadata," is withdrawn along with the branch.)*

**The honest one-line framing, updated:** *this is a port of a proven Go design to a language where nobody has done it, whose hard parts are genuinely Rust-specific, serving the platforms eBPF cannot reach.* Shorter than the previous framing by exactly the speculative research branch that has since been resolved upstream. That is a good project - arguably a better-defined one than before, since its scope no longer depends on an unvalidated hypothesis. It is not a novel-mechanism project, and describing it as one would not survive contact with someone who knows `otelc` exists.

### 15.2 Recommended architecture

**Architecture A - now the whole project, not a stage of it** ([Appendix D.4](appendix-d-maintainer-qa.md) closed Architectures C and D):

```
cargo metadata ─► rule matching ─► plan ─► pre-build runtime crate standalone
        │
        ▼
RUSTC_WRAPPER (--target-dir target/instrumented) ─► per-crate byte-range
        splice of extern "C" trampolines  __otel_span_enter / __otel_span_exit
        (syn for analysis only) ─► stock stable rustc
        │
        ▼
opentelemetry::trace  (NATIVE API - span kind, links, status)
        async sites wrapped via FutureExt::with_context
              ─► opentelemetry_sdk ─► opentelemetry-otlp ─► OTLP
```

With one structural decision taken on day one: **the emitter is a seam** (§11.4). It has already paid for itself - the reversal from `tracing` to the native OTel API ([Appendix D.2](appendix-d-maintainer-qa.md)) landed as an emitter swap rather than a rewrite.

### 15.3 Phase 1 starting point

In order:

1. **O1 is done - implement its confirmed mitigation first.** `RUSTC_WRAPPER` does *not* participate in Cargo's fingerprint (Appendix B item 2), and a `RUSTFLAGS`-based fix was shown to be destructive (Appendix C.3). The first thing to build is not an experiment but the isolated `--target-dir` mechanism itself, since everything after this step silently produces uninstrumented binaries without it.
2. **Walking skeleton.** A `RUSTC_WRAPPER` binary that logs its arguments and execs the real `rustc`, building into that isolated target dir. Confirm it is invoked for the crates we expect, including at least one dependency that does not declare our runtime crate.
3. **One hardcoded splice, across the crate boundary.** Splice an `extern "C"` trampoline call into a single named function in a dependency crate - not just the app - using `syn` for analysis and a byte-range insertion for the edit (Appendix C.2/C.6). Compile. See a span in an in-process collector, produced from inside the unmodified dependency.
4. **Async correctness before anything else.** **[Revised, [Appendix D.2](appendix-d-maintainer-qa.md)]** Implement MVP success criterion 4 - **exactly one span, duration ≈ *D*, and the context absent from the polling thread while the task is suspended** - plus criterion 5 (concurrent isolation, including worker-thread migration), as tests, and make them pass. *(The old "busy ≈ 0" half is dropped: `busy`/`idle` was a `tracing-opentelemetry` synthesis with no field in the OTel data model.)* Everything after this is easier; nothing after this matters if these are wrong.
5. **[Updated - [Appendix E](appendix-e-experiment-matrix.md) E-7/E-8/E-9/E-11]** ~~Close the four experiments that can still invalidate the architecture~~ - **done, individually.** Trampolines survive LTO/`panic=abort` (E-7); a `core`-only Tier-2 async wrapper is demonstrated correct under forced interleaving with a genuine positive control (E-8); the mechanism resolves on Linux, not just Windows (E-9); source-tree mirroring survives a real multi-file corpus, 7/8 after a splicer-placement fix (E-11). **What remains before generalising: FE-13 - run all four together**, through the real byte-range splicer rather than hand-written stand-ins, on one pipeline, per crate. Every result above validated one mechanism in isolation; none validated the combination the MVP actually ships. FE-3 (`#![forbid(unsafe_code)]` escape) and macOS (the second half of the old FE-7) remain open on their own.
6. **Then, and only then, generalise.** Rule format, matcher engine, `--plan-only`, exclusion list, snapshot tests.
7. **Then the corpus.** Run `--plan-only` over five real crates; fix what breaks.
8. **Then measure.** Build time, binary size, runtime. Publish.

**[Inference]** Steps 1 and 4 are ordered deliberately ahead of the interesting work. R1 and R5 are the two Critical risks that produce *silently wrong* behaviour, and both are cheap to test early and expensive to discover late. **[Revised by the Phase 0 completion audit, then again after E-7/E-8/E-9/E-11]** Step 5's individual falsifiers all held - none broke the architecture. The residual risk moved from "does each piece work" to "do they work assembled," which is a smaller but real question (FE-13) worth closing before the rule engine makes the pipeline harder to isolate for testing.

### 15.4 Deferred work

| Deferred to | Item |
| --- | --- |
| **Phase 2** | Third-party dependency instrumentation (`RUSTC_WRAPPER` in full scope) |
| **Phase 2** | `wrap_call` rules; `tokio::spawn` context propagation |
| **Phase 2** | Library-specific rules (axum/hyper server spans, reqwest client spans) with semantic-convention attributes |
| **Phase 2** | Cross-process W3C context propagation |
| **Phase 2** | Third-party-distributable instrumentation crates (the `otelc` ADR-0005 analogue) |
| **Phase 2** | **[New, [Appendix D.3](appendix-d-maintainer-qa.md)]** Automated latest-version compatibility CI over instrumented crates (the `otelc` #406 pattern) - required once we ship rules for more than a handful of crates |
| **Phase 2** | **[Updated - [Appendix E](appendix-e-experiment-matrix.md) E-8]** **Tier-2 async instrumentation** - the `core`-only spliced future wrapper that lets an `async fn` inside a dependency reproduce `FutureExt::with_context`'s lifecycle over the C ABI ([§16.3](16-instrumentation-semantics.md), [R25](13-technical-risks.md)). The mechanism is now demonstrated correct (E-8), not merely designed; still not on the MVP's critical path, since integration through the real splicer (FE-13) is separate work and the Phase 1 dependency slice stays synchronous |
| **Phase 2** | **[New, Phase 0 completion audit]** Reaching `#![forbid(unsafe_code)]` crates, if [Appendix E](appendix-e-experiment-matrix.md) FE-3 shows `unsafe extern { safe fn … }` is a viable route ([R26](13-technical-risks.md)). Until then they are skipped, and the coverage cost is reported as a number rather than assumed away |
| ~~**Phase 3**~~ | ~~Metadata emission as a first-class build artifact (build-ID-keyed)~~ **Dropped** - superseded by USDT ([Appendix C.4](appendix-c-adversarial-review.md)), then made moot by the branch closure |
| **Phase 3 (spike)** | MIR-level instrumentation research, isolated behind a nightly-only feature flag; primarily to test the §6.3 hypothesis about pre-`StateTransform` injection |
| ~~**Phase 4 (gated)**~~ | ~~eBPF loader consuming metadata - only if H2 survives §15.6~~ **ABANDONED - [Appendix D.4](appendix-d-maintainer-qa.md).** Being built upstream (OBI #1096). Replaced by: *review Giuseppe Ognibene's upstream PR and contribute the §6.3/§7 Rust async-semantics analysis* - an ongoing collaboration, not a project phase |
| **Never (unless a user asks)** | Metrics, logs, our own exporter, our own span type, `std` instrumentation, closure instrumentation, **anything eBPF** |

### 15.5 Open questions requiring experimental validation

Ordered by how much they change the plan. **[Consolidated by the Phase 0 completion audit]** Three separate open-question lists had accumulated - this one, [Appendix C.9](appendix-c-adversarial-review.md) (adversarial round), and [Appendix D.6](appendix-d-maintainer-qa.md) (maintainer round) - with overlapping and partly stale membership. **[Appendix E.3](appendix-e-experiment-matrix.md) is now the single consolidated list of unrun experiments**, and the earlier lists are annotated against it. Of C.9's seven: Q3, Q4 and Q5 closed with the eBPF branch ([ADR-005](17-decision-records.md)); Q6 (`fastrace` vs. `tracing`) was superseded by D-Q2 when `tracing` stopped being the baseline; Q1, Q2 and Q7 survive as FE-8 and FE-1.

| # | Question | Blocks | How to test |
| --- | --- | --- | --- |
| Q1 | ~~Does `RUSTC_WRAPPER` participate in Cargo's fingerprint?~~ **RESOLVED** - no, confirmed by direct experiment | Everything (R1) | **Done.** See [Appendix B](appendix-b-verification-log.md) item 2. Remaining work is implementation, not investigation: build the **isolated `--target-dir`** mechanism (**not** the `RUSTFLAGS`-hash mitigation, which [Appendix C.3](appendix-c-adversarial-review.md) showed to be destructive - it evicts the whole workspace cache and clobbers `.cargo/config.toml` rustflags) and cover it with a CI regression test that toggles the ruleset with no source change and asserts a rebuild occurred |
| Q2 | **[Reframed, [Appendix D.2](appendix-d-maintainer-qa.md)]** Can we produce correct async span semantics with generated `FutureExt::with_context` wrapping? (Was: "purely by generated `#[instrument]`") | The MVP's value | MVP criteria 4 and 5 - one span per future, duration ≈ wall clock, context absent while suspended, isolation under worker-thread migration |
| Q3 | ~~`syn`+`prettyplease` or `ra_ap_syntax`: which preserves enough fidelity to rewrite real third-party crates?~~ **RESOLVED - neither** | Phase 2 (O2, R11, R16) | **Done, [Appendix C.6](appendix-c-adversarial-review.md).** `syn`+`prettyplease`'s damage was measured (Appendix B item 7: all non-doc comments destroyed, whole-file reformat), and `cargo-mutants`' technique removes the choice entirely: `syn` for **analysis only**, then splice bytes at `span().byte_range()` into the original UTF-8 buffer. Formatting, comments, and line numbers survive byte-for-byte outside the insertion. `ra_ap_syntax` needs no evaluation. **What remains is a different question** - not "which parser," but "does source-tree *mirroring* survive real multi-file crates" ([Appendix D.6](appendix-d-maintainer-qa.md) D-Q4), which is now the project's largest open unknown |
| Q4 | What fraction of a real crate's functions do our default exclusions remove? | Whether the tool is useful at all | Instrument the corpus; report the skip-reason distribution |
| Q5 | What is the incremental build overhead? | Adoption | §14.3 |
| Q6 | **[Reframed, [Appendix D.2](appendix-d-maintainer-qa.md)]** ~~Does `#[instrument]` compose with `#[async_trait]`/`#[tokio::main]`, and in what attribute order?~~ Does a **body splice** survive functions that attribute macros rewrite? | Real-world compatibility (O4, [R6](13-technical-risks.md)) | Compilation matrix ([Appendix E](appendix-e-experiment-matrix.md) FE-11). A *different and probably easier* question than the original: we insert statements into a body rather than adding an attribute whose expansion order matters. What must survive is a macro that rewrites the body we spliced into - `#[async_trait]` boxing the future being the main case |
| Q7 | Can a span guard be threaded through a coroutine body *before* `StateTransform` without breaking borrowck or the transform? | Whether Architecture B has any async story at all | Phase 3 nightly spike |
| Q8 | ~~**(H2)** Are logical async spans reconstructible from poll-level uprobe events plus compile-time state-machine metadata?~~ **RESOLVED UPSTREAM - CLOSED** | ~~The entire eBPF branch~~ - branch abandoned | **Done, by someone else.** [Appendix D.4](appendix-d-maintainer-qa.md): OBI maintainer Giuseppe Ognibene has a working Tokio async task reconstruction and context propagation prototype (OBI #1096), in final testing against task migration and pointer reuse. The prototype this row scheduled will not be built here |
| Q9 | ~~Does v0 mangling already supply most of what our metadata would?~~ **CLOSED - no longer load-bearing** | ~~Whether metadata is a contribution or a convenience~~ | The question existed to decide whether compiler-emitted metadata for eBPF was a research contribution. With the eBPF branch abandoned ([Appendix D.4](appendix-d-maintainer-qa.md)) we emit no such metadata, so the comparison has nothing to inform. Still a genuinely interesting question - and now one worth raising in #1096's design discussion rather than answering here |
| Q10 | ~~What is the current resolution state of opentelemetry-rust#1571?~~ **RESOLVED, and its follow-on question now also closed** | Emitter choice | **Done.** Closed 2026-03-18 as "maintain both APIs," not deprecating `tracing`. The follow-on this row raised - that `docs/traces.md` recommends the OTel API for new code, putting us in knowing divergence from upstream - **is closed by [Appendix D.2](appendix-d-maintainer-qa.md): we reversed and now generate the native OTel API.** No divergence remains to re-confirm |

**[Inference - updated]** Q8 and Q9 were framed as deciding "whether the project's research half is real." Both are now closed without our running them: Q8 answered upstream, Q9 made moot by that answer. **The project no longer has a speculative research half, and its remaining open questions are all engineering questions with known methods** - chiefly Q3/Q4 here and D-Q4 in [Appendix D.6](appendix-d-maintainer-qa.md) (does source-tree mirroring survive real multi-file crates), which is now the largest single unknown and should be attacked first.

### 15.6 Conditions for abandoning or changing course

**Abandon the compile-time approach if:**

- **[Revised twice - post-verification, then [Appendix C.3](appendix-c-adversarial-review.md)]** Q1 is resolved: `RUSTC_WRAPPER` does *not* participate in Cargo's fingerprint, and the mitigation is an **isolated `--target-dir`** (the `RUSTFLAGS`-hash alternative was shown to be destructive and is not used). The abandon condition narrows accordingly: abandon only if the isolated target dir itself fails to generalize - e.g. if it breaks under `RUSTC_WORKSPACE_WRAPPER` combined with a separate `RUSTC_WRAPPER`, or if the permanent loss of cache sharing with the user's default `target/` makes rebuild times intolerable in practice. This should surface early in Phase 1 (§15.3 step 1) if it is going to happen at all.
- Q2 shows correct async span semantics are unachievable via generated code. **[Revised, [Appendix D.2](appendix-d-maintainer-qa.md)]** This would now mean `FutureExt::with_context` - a documented, maintainer-endorsed part of the OTel Rust API - is broken, which is implausible, but it remains the load-bearing assumption.
- Q3 shows source rewriting cannot survive contact with real third-party crates. **[Re-sharpened]** The *parser* half of this is closed: byte-range splicing preserves everything outside the insertion ([Appendix C.6](appendix-c-adversarial-review.md)), so "no viable stable-Rust rewriting tool exists" is no longer a live failure mode. What could still fail is **source-tree mirroring** against real crate layouts - `include!`, `#[path]`, `build.rs`-generated modules, `sqlx::query!` ([Appendix D.6](appendix-d-maintainer-qa.md) D-Q4). If a corpus run shows most real dependencies cannot be mirrored and spliced safely, narrow the project to workspace-only instrumentation and say so plainly, rather than reconsidering Architecture B - the nightly pin remains unshippable regardless.
- Build overhead proves so large that no one would run it. **[Anchored to real data, [Appendix D.3](appendix-d-maintainer-qa.md)]** `otelc` measures +275% on a small single-package build and +54% multi-package, so 1.5×–3× clean compile is the expected range and is *not* by itself an abandon signal. The threshold that matters is incremental rebuild: consistently >2× on a one-line change is the number users will not tolerate. **Note the fallback has changed** - the old "then pivot toward the metadata/eBPF branch, which has no build cost" is gone with that branch ([Appendix D.4](appendix-d-maintainer-qa.md)). The remaining lever is scope: fewer crates instrumented by default, better splice caching, opt-in dependency coverage.

**Abandon the eBPF branch if:** - ⛔ **CONDITION TRIGGERED AND EXECUTED, 2026-09-04. The branch is abandoned.**

> **[Fact - [Appendix D.4](appendix-d-maintainer-qa.md).]** The third condition below fired, in its strongest form. OBI maintainer **Giuseppe Ognibene has a working prototype** of Tokio async task reconstruction and context propagation in eBPF, tracked under **OBI issue #1096**, in final testing against task migration across worker threads and pointer reuse. OBI maintainer Nikola Grcevski separately confirmed OBI's current Rust support remains generic socket kprobes with zero application-level uprobes - i.e. #1096 is the work that changes that, and it is happening in the canonical upstream project.
>
> **Action taken, per this section's own instruction to "contribute there instead of competing":**
> - Architectures C and D abandoned ([§10](10-architecture-candidates.md)); no eBPF loader, sidecar, or competing implementation will be built.
> - Q8/H2 closed ([§15.5](15-final-recommendation.md), [§7.5](07-ebpf-future.md)); R19–R21 closed ([§13](13-technical-risks.md)).
> - Phase 4 removed from [§15.4](15-final-recommendation.md), replaced by ongoing collaboration: review Giuseppe's upstream PR, contribute the §6.3/§7 Rust async-semantics analysis to the edge cases still in testing.
> - The project focuses 100% on Architecture A, positioned where eBPF structurally cannot go: **macOS, Windows, unprivileged containers, non-root deployments.**
>
> This is the outcome to want. The research question was real, it was answered by someone better placed to answer it, and it was answered before a phase was spent on it. Architecture A was deliberately chosen to have standalone value precisely so this closure would cost nothing structural - see the last bullet of "Do not abandon merely because," written before this happened.

The original conditions, retained as the record of what was being watched for:

- ~~Q8's first sub-question fails: no stable task identity is observable from eBPF.~~ Overtaken - the identity question was settled affirmatively in practice by a working prototype, not by our test.
- ~~Q9 shows v0 demangling already supplies the bulk of the useful metadata.~~ Moot; no metadata will be emitted.
- **OBI ships semantic Rust function-level instrumentation upstream** - **[THIS FIRED]**, via #1096. The condition anticipated a shipped release; what arrived was better: an in-progress prototype with a reachable, receptive maintainer, which is a contribution opportunity rather than a fait accompli.

**Change course toward contribution rather than construction if:**

- The OTel Rust SIG announces a compile-time instrumentation effort. **[Fact]** Rust's absence from the zero-code list is conspicuous and OTel has now shipped this for Go; someone starting it is a realistic possibility within the project's lifetime. Contributing to an official effort is a better outcome than a parallel personal tool, and the research in this document transfers directly.

**Do not abandon merely because:**

- The mechanism is not novel. It is not, and that was known from §2. Usefulness and novelty are separate axes, and the useful half is the half that is missing.
- `otelc` exists. That is evidence the design works, not evidence the Rust work is redundant.
- ~~The eBPF branch dies.~~ **It has now died, and this bullet held.** Architecture A has standalone value and was chosen specifically so that it would. The closure cost the project one deferred phase and no structural rework ([Appendix D.4](appendix-d-maintainer-qa.md)).

### 15.7 Research-paper readiness

Assessed honestly, because the answer changed when [ADR-005](17-decision-records.md) closed the eBPF branch and the project lost the half that carried its research claim.

**The rule this assessment applies:** *a tool not existing is not novelty.* "Nobody has built X" is a market observation. A research contribution requires a **question whose answer was not known in advance** and a **method that could have produced a different answer**.

#### Three contributions, separated

| | Claim | Strength |
| --- | --- | --- |
| **Engineering contribution** *(real, and the reason to build)* | A working, stable-Rust, zero-code compile-time OTel instrumentation tool that reaches third-party dependencies - the empty cell in [§8](08-competitive-landscape.md)'s table, and the one thing a user cannot achieve with an afternoon and a text editor ([§9.5](09-gap-analysis.md)) | **Strong.** Verified gap, proven mechanism, clear user |
| **Research contribution** *(narrow, and it is not the mechanism)* | An account of what changes when compile-time auto-instrumentation is ported to a language with (a) no runtime, (b) lazy futures compiled into anonymous state machines, (c) monomorphization, (d) macro-invisible code, and (e) no `//go:linkname`. Plus **the first published runtime-overhead numbers for Rust auto-instrumentation** - `otelc` publishes compile-time figures only, and OBI publishes none for Rust ([Appendix D.3](appendix-d-maintainer-qa.md)) | **Modest but genuine.** A systems-experience or tool paper, not a novel-mechanism paper |
| **Potential future contribution** *(not ours)* | Compiler-emitted async state-machine metadata; eBPF reconstruction of Tokio task structure | **Withdrawn.** Mechanism not novel (USDT, [Appendix C.4](appendix-c-adversarial-review.md)); content being built upstream (OBI #1096, [Appendix D.4](appendix-d-maintainer-qa.md)) |

#### Is it publishable?

**As a tool/experience paper: plausibly, once the numbers exist.** Not as a novel-mechanism paper, and attempting one would not survive a reviewer who knows `otelc` exists ([§9.4](09-gap-analysis.md)).

What such a paper would need, and what it already has:

| Requirement | Status |
| --- | --- |
| Research question | ✅ Stated ([§14.7](14-evaluation-plan.md)) |
| Falsifiable hypotheses | ✅ HA–HF, each with its falsifier ([§14.7](14-evaluation-plan.md)) |
| Independent / dependent variables | ✅ Enumerated ([§14.7](14-evaluation-plan.md)) |
| Baselines | ✅ Five, including manual instrumentation by hand - the demanding one ([§14.7](14-evaluation-plan.md)) |
| Workloads | ✅ W1–W3 fixed and published ([§14.7](14-evaluation-plan.md)) |
| Correctness criteria | ✅ A formal oracle, not a vibe ([§16](16-instrumentation-semantics.md)) |
| Performance / build-time criteria | ✅ Specified; **anchored to real `otelc` figures** rather than invented targets ([§14.3](14-evaluation-plan.md)) |
| Dependency-coverage criteria | ✅ Specified, including the **negative** measures - exclusion rates, mirroring failures, `forbid(unsafe_code)` blocks ([§14.7](14-evaluation-plan.md)) |
| **Results** | ❌ **None.** Every table in [§14](14-evaluation-plan.md) is empty by construction |

**[Inference]** The honest position: **Phase 0 has produced a publishable *design and method*, and zero results.** That is the correct state at the end of Phase 0, and it is worth saying plainly rather than dressing the design up as a finding. The strongest paper available is *"we built the thing that was missing, here is what Rust made hard, and here are the first real numbers"* - and the third clause is the part that does not exist yet.

**One asset that is unusual and worth keeping:** the [Appendix B](appendix-b-verification-log.md) → [C](appendix-c-adversarial-review.md) → [D](appendix-d-maintainer-qa.md) → [E](appendix-e-experiment-matrix.md) chain records four rounds in which stated positions were **overturned by evidence** - twice by experiment, once by primary source, once by maintainer testimony - including the reversal of this document's own central emitter decision. Most tool papers present a design as though it arrived finished. Presenting the falsification path, including the claims that did not survive, is both more useful to a reader and harder to fake.

---

---

← [Evaluation Plan](14-evaluation-plan.md) · [Contents](../../README.md) · [Instrumentation Semantics](16-instrumentation-semantics.md) →
