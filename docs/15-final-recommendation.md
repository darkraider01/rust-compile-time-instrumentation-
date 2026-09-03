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
- Treating eBPF as a planned phase. It rests on H2 (logical async spans reconstructible from poll events plus metadata), which is unvalidated. **[Revised — see [Appendix C.5](appendix-c-adversarial-review.md)]** The original basis for doubting H2 — "Rust and Tokio expose no stable task identity" — was itself factually wrong (`tokio::task::Id` is stable). The real constraint is narrower and still open: an *external* eBPF observer must recover that identity from the task's in-memory layout at a known offset, not from the stable Rust API, which is exactly the struct-offset fragility already observed in existing prior art (§4.5).
- Claims of novelty for the *mechanism*. Compile-time auto-instrumentation is a solved, shipped idea in Go, and compile-time metadata embedded in a binary for eBPF consumption is a two-decade-old idea (USDT) with mature stable-Rust implementations (Appendix C.4). **The novelty is in the Rust adaptation — specifically async semantics, monomorphization, and macro invisibility for the compile-time half, and specifically *async state-machine structure* (not metadata in general) for the eBPF half — not in either mechanism as a concept.**

**The honest one-line framing:** *this is a port of a proven Go design to a language where nobody has done it, whose hard parts are genuinely Rust-specific, with one speculative research question (compiler metadata for eBPF) attached as an optional later branch.* That is a good project. It is not a novel-mechanism project, and describing it as one would not survive contact with someone who knows `otelc` exists.

### 15.2 Recommended architecture

**Architecture A**, structured so Architecture D is reachable by addition:

```
cargo metadata ─► rule matching ─► plan ─► pre-build runtime crate standalone
        │
        ▼
RUSTC_WRAPPER (--target-dir target/instrumented) ─► per-crate byte-range
        splice of extern "C" trampolines (syn for analysis only) ─► stock stable rustc
        │
        ▼
tracing (otel.kind/otel.status_code) ─► tracing-opentelemetry ─► opentelemetry_sdk ─► OTLP
```

With one structural decision taken on day one: **the emitter is a seam** (§11.4), so "emit metadata instead of code" is a configuration, not a rewrite.

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
| **Phase 3** | Metadata emission as a first-class build artifact (build-ID-keyed) |
| **Phase 3 (spike)** | MIR-level instrumentation research, isolated behind a nightly-only feature flag; primarily to test the §6.3 hypothesis about pre-`StateTransform` injection |
| **Phase 4 (gated)** | eBPF loader consuming metadata — **only if H2 survives §15.6** |
| **Never (unless a user asks)** | Metrics, logs, our own exporter, our own span type, `std` instrumentation, closure instrumentation |

### 15.5 Open questions requiring experimental validation

Ordered by how much they change the plan.

| # | Question | Blocks | How to test |
| --- | --- | --- | --- |
| Q1 | ~~Does `RUSTC_WRAPPER` participate in Cargo's fingerprint?~~ **RESOLVED** — no, confirmed by direct experiment | Everything (R1) | **Done.** See [Appendix B](appendix-b-verification-log.md) item 2. Remaining work is implementation, not investigation: build the `RUSTFLAGS`-hash cache-busting mitigation and cover it with a regression test in CI (a test that toggles the ruleset with no source change and asserts a rebuild occurred) |
| Q2 | Can we produce correct async span semantics purely by generated `#[instrument]`? | The MVP's value | MVP criteria 4 and 5 |
| Q3 | `syn`+`prettyplease` or `ra_ap_syntax`: which preserves enough fidelity to rewrite real third-party crates? **Partially resolved** | Phase 2 (O2, R11, R16) | `syn`+`prettyplease`'s cost is now measured (Appendix B item 7: drops all non-doc comments, reformats the whole file, idempotent). Remaining: prototype `ra_ap_syntax` and compare against this now-known baseline, and round-trip both over a real third-party corpus |
| Q4 | What fraction of a real crate's functions do our default exclusions remove? | Whether the tool is useful at all | Instrument the corpus; report the skip-reason distribution |
| Q5 | What is the incremental build overhead? | Adoption | §14.3 |
| Q6 | Does `#[instrument]` compose with `#[async_trait]`, `#[tokio::main]`, and friends, and in what attribute order? | Real-world compatibility (O4, R6) | Compilation matrix |
| Q7 | Can a span guard be threaded through a coroutine body *before* `StateTransform` without breaking borrowck or the transform? | Whether Architecture B has any async story at all | Phase 3 nightly spike |
| Q8 | **(H2)** Are logical async spans reconstructible from poll-level uprobe events plus compile-time state-machine metadata? | The entire eBPF branch | Prototype: attach uprobes to a known async binary, attempt reconstruction with hand-written metadata. First sub-question: **is there any stable task identity observable from eBPF?** If no, H2 is dead. **New evidence, not resolution:** [J00MZ/opentelemetry-rust-instrumentation](https://github.com/J00MZ/opentelemetry-rust-instrumentation) is already attempting this via an "instrument at the executor level, track task contexts" heuristic with no published accuracy data — worth reading in full before prototyping from scratch (§4.5, §7.5) |
| Q9 | Does v0 mangling already supply most of what our metadata would? | Whether metadata is a contribution or a convenience | Demangle a real binary's symbols; compare recoverable information against our proposed metadata schema, field by field. **Sharpened by verification:** the comparison baseline is not OBI, which does no Rust function-level probing at all (confirmed: it uses kprobes/socket filters for Rust, not uprobes — Appendix B item 6), but projects like J00MZ that already do symbol-based probe selection by hand |
| Q10 | ~~What is the current resolution state of opentelemetry-rust#1571?~~ **RESOLVED** | Emitter choice (R15) | **Done.** Closed 2026-03-18 as "maintain both APIs," interop-focused, not deprecating `tracing`. **New question surfaced by resolving this one:** opentelemetry-rust's own `docs/traces.md`, added after #1571 closed, explicitly recommends the OTel API directly for new code — our choice to generate `tracing` now knowingly diverges from current upstream guidance (§5.2, §5.4). Re-confirm this is still the right call closer to Phase 1 implementation, since traces API stability may have changed by then |

**[Inference]** Q8 and Q9 together decide whether the project's research half is real. Q9 in particular is cheap — a few hours with `rustfilt` and a real binary — and could substantially deflate the metadata hypothesis. **It should be done early, precisely because it might tell us something we do not want to hear.**

### 15.6 Conditions for abandoning or changing course

**Abandon the compile-time approach if:**

- **[Revised post-verification]** Q1 is resolved — `RUSTC_WRAPPER` does *not* participate in Cargo's fingerprint by default, and a `RUSTFLAGS`-hash cache-busting workaround was experimentally confirmed to work. The abandon condition narrows accordingly: abandon only if that mitigation itself fails to generalize in practice — e.g., if it interacts badly with users' own `RUSTFLAGS`, breaks under `RUSTC_WORKSPACE_WRAPPER` combined with a separate `RUSTC_WRAPPER`, or defeats incremental compilation broadly rather than just correctly forcing rebuilds when the ruleset changes. This should surface early in Phase 1 (§15.3 step 1) if it is going to happen at all.
- Q2 shows correct async span semantics are unachievable via generated code. (This would mean `#[instrument]` itself is broken, which is implausible, but it is the load-bearing assumption.)
- Q3 shows source rewriting cannot survive contact with real third-party crates. **[Sharpened]** We already know `syn`+`prettyplease` destroys comments and reformats whole files (Appendix B item 7) — that alone is not disqualifying, since it does not prevent correct compilation. Abandon this architecture specifically if `ra_ap_syntax` *also* cannot survive real crates, since at that point no available stable-Rust source-rewriting tool is viable. Then reconsider Architecture B, accepting the nightly pin, *or* narrow the project to workspace-only instrumentation and admit it is `tracing-orchestra` done properly.
- Build overhead proves so large (say, incremental builds consistently >2× slower) that no one would run it. Then pivot toward the metadata/eBPF branch, which has no build cost.

**Abandon the eBPF branch if:**

- Q8's first sub-question fails: no stable task identity is observable from eBPF. Then logical async spans cannot be reconstructed, and "compiler-assisted eBPF" reduces to a nicer uprobe configuration format. Write it up as a negative result and stop. **[Note]** [J00MZ/opentelemetry-rust-instrumentation](https://github.com/J00MZ/opentelemetry-rust-instrumentation)'s "executor-level" heuristic (§7.5) is worth reading before concluding this independently — if it has already hit this wall, that is directly usable negative evidence.
- Q9 shows v0 demangling already supplies the bulk of the useful metadata. Then the contribution is a configuration convenience, not a technical advance — still publishable as a small tool, not worth a project phase.
- OBI ships semantic Rust function-level instrumentation upstream, **or** the J00MZ project (or a successor) is adopted into the `open-telemetry` org and reaches usable maturity. **[Sharpened]** We now know J00MZ's `Cargo.toml` already declares the `open-telemetry/opentelemetry-rust-instrumentation` repository path as its intended home (though that repository does not yet exist) — this is a closer and more concrete possibility than a generic "someone ships this upstream" risk. Then contribute there instead of competing.

**Change course toward contribution rather than construction if:**

- The OTel Rust SIG announces a compile-time instrumentation effort. **[Fact]** Rust's absence from the zero-code list is conspicuous and OTel has now shipped this for Go; someone starting it is a realistic possibility within the project's lifetime. Contributing to an official effort is a better outcome than a parallel personal tool, and the research in this document transfers directly.

**Do not abandon merely because:**

- The mechanism is not novel. It is not, and that was known from §2. Usefulness and novelty are separate axes, and the useful half is the half that is missing.
- `otelc` exists. That is evidence the design works, not evidence the Rust work is redundant.
- The eBPF branch dies. Architecture A has standalone value and was chosen specifically so that it does.

---

---

← [Evaluation Plan](14-evaluation-plan.md) · [Contents](../README.md) · [Appendix A — Sources](appendix-a-sources.md) →
