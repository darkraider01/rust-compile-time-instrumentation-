← [Instrumentation Semantics](16-instrumentation-semantics.md) · [Contents](../../README.md) · [Appendix A - Sources](appendix-a-sources.md) →

---

## 17. Architecture Decision Records

The research sections argue; this section decides. Each record states what was chosen, what it was chosen over, what evidence forced the choice, what it costs, and **what would make us revisit it**. The revisit conditions are the important half - a decision with no falsifier is a preference.

Records are written in the order the decisions became final, which is not the order they were first considered. Three of the six reverse an earlier position; that history is preserved deliberately ([Appendix B](appendix-b-verification-log.md), [C](appendix-c-adversarial-review.md), [D](appendix-d-maintainer-qa.md)), because the reversal path is evidence about the process, not embarrassment to be tidied away.

| ADR | Decision | Status | Reverses |
| --- | --- | --- | --- |
| [001](#adr-001--generate-native-opentelemetry-api-calls) | Generate native OpenTelemetry API calls, not `tracing` | **Accepted** | Yes - §5.4's original recommendation |
| [002](#adr-002--surgical-byte-range-splicing-not-pretty-printing) | Surgical byte-range splicing, not `syn`→`prettyplease` | **Accepted** | Yes - §12.4's original design |
| [003](#adr-003--extern-c-trampolines-for-dependency-coverage) | `extern "C"` trampolines for dependency coverage | **Accepted** | Yes - the injected-crate-dependency design |
| [004](#adr-004--isolated-target-dir-for-cache-correctness) | Isolated `--target-dir`, not `RUSTFLAGS` | **Accepted** | Yes - Appendix B item 2's proposed mitigation |
| [005](#adr-005--the-ebpf-branch-is-closed) | The eBPF branch is closed | **Accepted** | Retires Architectures C and D |
| [006](#adr-006--the-emitter-is-a-seam) | The emitter is a seam | **Accepted** | No - held since §11.4, and vindicated by 001 |

---

### ADR-001 - Generate native OpenTelemetry API calls

**Status:** Accepted, 2026-09-04. Reverses the original recommendation in [§5.4](05-otel-rust.md).

#### Context

The tool must decide what code it generates at an instrumentation site. Rust has two sanctioned span APIs - the `opentelemetry` crate's own tracing API, and the `tracing` crate bridged via `tracing-opentelemetry` - and [opentelemetry-rust#1571](https://github.com/open-telemetry/opentelemetry-rust/issues/1571) resolved as **"maintain both"**, so neither is going away ([§5.2](05-otel-rust.md)). The choice is ours to make and to justify.

#### Options considered

1. **Generate `#[tracing::instrument]` / `tracing::span!`**, bridged to OTel. Originally chosen.
2. **Generate native OpenTelemetry API calls**, with `opentelemetry::trace::FutureExt::with_context` for async. Chosen.
3. **Generate `fastrace`** ([Appendix C.7](appendix-c-adversarial-review.md)). Rejected as a default: a third ecosystem, and its "10–100×" figure is its own tagline, not a measurement of our workload.
4. **Invent an abstraction over both.** Rejected outright - a third span model is exactly what [§11.3](11-recommended-architecture.md) says not to build.

#### Evidence

- **[Fact]** `opentelemetry::trace::FutureExt::with_context` wraps any `Future`, attaching the context on each `poll()` and detaching on yield across threads - the same lifecycle as `tracing::Instrument` ([§5.3](05-otel-rust.md)).
- **Maintainer, Scott Gerring (`#otel-rust`):** *"If your goal is compile-time instrumentation, I don't see async future handling as a reason to favour the tracing api."* ([Appendix D.2](appendix-d-maintainer-qa.md))
- **[Fact]** opentelemetry-rust's own `docs/traces.md`: *"For new code, prefer the OpenTelemetry Tracing API directly."*
- **Maintainer, same source:** `tracing-opentelemetry`'s context-synchronisation bridge is *"super hairy / hot-path-y / full of terror"* - reframing the crate from free maintenance relief into an un-understood hot-path dependency.
- **[Fact, [Appendix C.1](appendix-c-adversarial-review.md)]** `tracing`'s `STATIC_MAX_LEVEL` is global and additive, so it was never the per-tool kill switch option 1 credited it with being.
- **[Fact, [Appendix C.1](appendix-c-adversarial-review.md)]** Span **links** are the one OTel capability the `tracing` bridge genuinely cannot express.

The load-bearing argument for option 1 was that only `tracing` models async interleaving correctly. That argument was false. **The other five reasons ([§5.4](05-otel-rust.md)) do not carry the decision without it.**

#### Decision

**Generate native OpenTelemetry API calls. Wrap instrumented futures with `FutureExt::with_context`. Do not generate `tracing` spans by default.**

#### Consequences

- ✅ **Follows** upstream guidance instead of documenting a disagreement with it.
- ✅ **Three injected dependencies removed** - `tracing`, `tracing-subscriber`, `tracing-opentelemetry` ([§12.4](12-mvp-definition.md)). Closes [R14](13-technical-risks.md) and [R15](13-technical-risks.md).
- ✅ **Span kinds and links become first-class**, which Phase 2's `tokio::spawn` fan-out modelling needs.
- ❌ **Direct exposure to a Beta traces API** with no bridge absorbing churn - [R13](13-technical-risks.md), raised from Medium to High.
- ❌ **No `STATIC_MAX_LEVEL` analogue** - [R23](13-technical-risks.md). The `--cfg` gate remains a true compile-time deletion; what is lost is the *runtime-disabled-but-present* configuration being free.
- ❌ **No `busy`/`idle` split.** A `tracing-opentelemetry` synthesis with no field in the OTel data model; a diagnostic loss, not a correctness one.
- ⚠️ **A Tier-1/Tier-2 split appears** ([§16.3](16-instrumentation-semantics.md)): a dependency crate cannot name `opentelemetry`, so it cannot call `with_context` and needs an equivalent spliced form. This was not noticed until the semantics were written out, and it is the decision's largest unpriced consequence.

#### Revisit if

- `FutureExt::with_context` proves incorrect or unusably costly under auto-instrumentation span volume ([Appendix E](appendix-e-experiment-matrix.md) FE-5/FE-6).
- Measured per-span cost of the native API is materially worse than `tracing` + bridge for many small short-lived spans (D-Q2). The remedy is an emitter swap ([ADR-006](#adr-006--the-emitter-is-a-seam)), not a redesign.
- Upstream reverses `docs/traces.md`, or the traces API's Beta churn proves unmanageable in practice.

---

### ADR-002 - Surgical byte-range splicing, not pretty-printing

**Status:** Accepted, 2026-09-04. Reverses the original `syn`→`prettyplease` design.

#### Context

The tool edits Rust source before handing it to `rustc`. Users will read the result when a build breaks ([R12](13-technical-risks.md)), and compiler diagnostics point into it. How the edit is applied determines whether that output is debuggable.

#### Options considered

1. **`syn::parse_file` → transform AST → `prettyplease::unparse`.** Originally chosen.
2. **`ra_ap_syntax`** (rust-analyzer's lossless CST).
3. **`syn` for analysis only; edit the original UTF-8 buffer at `span().byte_range()`.** Chosen.

#### Evidence

- **[Fact - hands-on experiment, [Appendix B](appendix-b-verification-log.md) item 7]** A `syn` 2 + `prettyplease` 0.2 round-trip of a representative sample **destroyed all 4 non-doc `//` comments** (comments are not tokens and `syn` never sees them), preserved both doc comments, collapsed blank lines, and **reformatted the entire file** to canonical style - not just the touched item. Idempotent, but destructive on first pass. No semantic loss.
- **[Fact - primary source, [Appendix C.6](appendix-c-adversarial-review.md)]** `cargo-mutants` solves precisely this: *"The file is parsed using the syn crate, but mutations are applied textually, rather than to the token stream, so that unmutated code retains its prior formatting, comments, line numbers, etc."*

#### Decision

**Parse with `syn` for analysis only - locating targets, reading attributes, checking exclusions. Take `span().byte_range()` and splice into the original UTF-8 buffer. Never re-print the file.** Following `cargo-mutants`' proven technique.

#### Consequences

- ✅ Comments, formatting, and blank lines survive **byte-for-byte** outside the insertion point.
- ✅ **Line numbers are stable** ahead of the insertion, so `code.line.number` ([§16.14](16-instrumentation-semantics.md)) and compiler diagnostics stay meaningful. Substantially defuses [R11](13-technical-risks.md).
- ✅ `--dump-rewritten` produces a readable one-line diff rather than a whole-file reformat.
- ✅ **Removes option 2 from consideration entirely** - no `ra_ap_syntax` evaluation needed, one fewer heavy dependency.
- ✅ Cheaper than parse-and-print, so [§14.3](14-evaluation-plan.md)'s build-overhead figure should land below the original estimate.
- ❌ Insertions still shift line numbers **after** the insertion point within the same file. Small, bounded, and directly testable.
- ❌ Byte offsets must be applied **back-to-front** when a file has multiple sites, or earlier splices invalidate later ranges. An implementation trap worth naming.

#### Revisit if

- `syn`'s `byte_range()` proves unreliable on some construct - it is the single API the whole mechanism rests on.
- A future rule type needs a genuine structural rewrite (moving code, not inserting it), where textual splicing stops being expressive enough. No Phase 1 or Phase 2 rule does.

---

### ADR-003 - `extern "C"` trampolines for dependency coverage

**Status:** Accepted, 2026-09-04. Reverses the injected-crate-dependency design.

#### Context

**Dependency coverage is the differentiator.** [§9.5](09-gap-analysis.md): *"This is the single test that separates a real tool from a wrapper."* Instrumented code inside a third-party crate has to call *something*, but that crate does not declare our runtime, and Cargo will not pass `--extern` for a dependency it does not know about. An adversarial review called this a fatal structural flaw and claimed the architecture was *"impossible on stable Rust"* ([Appendix C](appendix-c-adversarial-review.md)).

#### Options considered

1. **Inject an undeclared crate dependency** - wrapper appends `-L dependency=… --extern otel_shim=…`.
2. **Inject only an `extern "C"` symbol declaration**, resolved at the application's final link. Chosen.
3. **Edit the dependency's `Cargo.toml`.** Rejected - mutating a registry crate's manifest and the user's lockfile is unacceptable.
4. **Vendor and patch dependencies.** Rejected - that is what users do today, and the reason the tool would exist.

#### Evidence

**[Fact - both mechanisms built and run on stable Rust 1.97.1, no `-Zunstable-options` ([Appendix C.2](appendix-c-adversarial-review.md))]** Three crates: `otel_shim` (runtime, pre-built standalone), `victim` (a dependency that does **not** declare `otel_shim`), `app` (depends only on `victim`).

- **Mechanism 1 worked**, refuting the review's headline claim - but revealed a constraint the review did not anticipate: the injected dependency propagates into crate metadata, so **every downstream consumer** also needs `-L`, or the build fails with `E0463`.
- **Mechanism 2 worked with no appended flags at all.** The wrapper spliced only `unsafe extern "C" { fn __otel_span_enter(name: *const u8, len: usize); }`; the symbol resolved at `app`'s final link, and the probe fired.
- **[Fact]** Mirrors `otelc`'s own trampoline indirection ([§2.5](02-otelc-go.md)), reached without Go's `//go:linkname`.
- **[Fact - [Appendix E](appendix-e-experiment-matrix.md) FE-1, second round]** "Resolve at the application's final link" is not automatic. `rustc` decides which `--extern` rlibs to pass to the linker based on whether any **Rust item path** into that crate is referenced anywhere in the compiled graph - this happens upstream of and independent from the linker's own dead-stripping. `victim`'s `extern "C"` block declares an untyped C symbol, not a path into `otel_shim`, so an application whose source never names an item in `otel_shim` gets `libotel_shim.rlib` silently dropped from the link line, producing `LNK2019`/`undefined reference` pointed at the *dependency*, not at the application. A `#[used]` static anchor does **not** fix this - it acts after rustc's pruning decision is already made. Only a genuine item-path reference (e.g. calling a runtime-init function) does.

#### Decision

**Splice only `extern "C"` symbol declarations into instrumented dependencies. Append no `--extern`, no `-L`, and no manifest edits. Resolve at the application's final link, from a runtime crate the application declares as an ordinary dependency. Pre-build that runtime standalone, outside Cargo's DAG.** Symbols standardised as `__otel_span_enter` / `__otel_span_exit`, with the full ABI in [§16.3](16-instrumentation-semantics.md).

**Hard requirement, not optional:** the application **must** contain a genuine Rust item-path reference into the runtime crate - the generated runtime-init call (`otel_shim::init()` or equivalent) satisfies this by construction, since [§11.1](11-recommended-architecture.md) step 6 already generates one. The tool **must preflight-check** that this reference exists before relying on the trampoline mechanism for any instrumented crate, and fail with a clear diagnostic naming the *application's* missing init call - not surface `rustc`'s `LNK2019` pointed at the dependency, which would be undiagnosable by a user who never touched that crate.

#### Consequences

- ✅ **Structurally cannot** produce duplicate-crate or dependency-cycle errors - the failure class that constrains Mechanism 1.
- ✅ **No `-L` propagation** to downstream crates.
- ✅ The dependency's `Cargo.toml` and the lockfile are **provably untouched** ([§12.9](12-mvp-definition.md) O7), which is MVP success criterion 1a.
- ✅ C ABI is stable across compiler versions and crate boundaries by definition.
- ✅ Pre-building the runtime standalone defeats the topological-scheduling objection: Cargo never learns the runtime exists.
- ❌ **The dependency cannot call `FutureExt::with_context`** - it cannot name `opentelemetry`. This forces the Tier-1/Tier-2 split ([§16.3](16-instrumentation-semantics.md)). **[Updated - [Appendix E](appendix-e-experiment-matrix.md) FE-2, second round]** A `core`-only spliced future wrapper reproducing the same lifecycle over an extended C ABI is now **demonstrated feasible** - first-poll span start, paired attach/detach, a validated detach token, tested under forced single-thread interleaving with both a positive control (a child span correctly parented to a live context) and the negative check (not parented to a suspended one). Not yet integrated: run through the real splicer into a real dependency, under LTO/`panic=abort`, cross-platform ([R25](13-technical-risks.md), downgraded from "unproven" to "mechanism demonstrated, integration untested").
- ❌ **`#![forbid(unsafe_code)]` crates cannot be instrumented at all** - `forbid` cannot be lifted by an inner `allow` (`E0453`). Phase 1 skips them and says so ([§16.3](16-instrumentation-semantics.md), [R26](13-technical-risks.md)). A possible escape via `unsafe extern { safe fn … }` (Rust 1.82+) is untested ([Appendix E](appendix-e-experiment-matrix.md) FE-3).
- ❌ The declaration's syntax is **edition-dependent** (`unsafe extern` is edition 2024), so the splicer must key on the `--edition` in the argv.
- ❌ **[New - FE-1]** The application must reference the runtime crate by a genuine Rust item path, or the mechanism silently fails to link (see Evidence above). Requires a preflight check; cannot be caught by the linker alone.
- ✅ **[Resolved - FE-1, FE-7]** Survives `lto = true` + `codegen-units = 1` + `panic = "abort"` on Windows (verified via disassembly of the unmodified release binary, no anchoring flags) and resolves cleanly on Linux/ELF/GNU-ld in both debug and that same release configuration, with no appended flags either way. **macOS remains untested** ([R24](13-technical-risks.md)).

#### Revisit if

- Tier-2 async, once spliced by the real mechanism into a real dependency rather than hand-written, fails under LTO or on a platform other than Windows.
- The share of the real dependency graph that forbids unsafe code turns out to be large enough to hollow out the differentiator (FE-3, then a corpus count).
- macOS's `ld64` strips the trampoline the way Windows/Linux linkers do not.

---

### ADR-004 - Isolated `--target-dir` for cache correctness

**Status:** Accepted, 2026-09-04. Reverses the `RUSTFLAGS`-hash mitigation proposed in [Appendix B](appendix-b-verification-log.md) item 2.

#### Context

**[Fact - hands-on experiment]** Cargo's rebuild fingerprint does **not** include whether `RUSTC_WRAPPER` is set. Building a crate, then enabling the wrapper with no source change, produces zero recompilation: Cargo prints `Finished`, the wrapper is never invoked for the actual compile, and the fingerprint hash is byte-identical. `RUSTC_WORKSPACE_WRAPPER` behaves the same ([Appendix B](appendix-b-verification-log.md) item 2).

This is the worst failure mode an observability tool can have - **it looks like it worked** - and it is the default steady state of a plain `cargo build`, not an edge case. It is [R1](13-technical-risks.md), the project's first Critical risk.

#### Options considered

1. **Derive a synthetic `RUSTFLAGS` value** (a `--cfg` carrying a rule-set hash) to force invalidation. Originally chosen.
2. **Build into an isolated `--target-dir`.** Chosen.
3. **Touch source files** to invalidate mtimes. Rejected - mutates the user's tree and registry sources are read-only.
4. **`cargo clean` before instrumented builds.** Rejected - destroys the user's entire cache on every run.

#### Evidence

- **[Fact]** Option 1 *works*: three consecutive `RUSTFLAGS` changes produced three consecutive recompiles ([Appendix B](appendix-b-verification-log.md) item 2, step 6).
- **[Fact - why it was rejected anyway, [Appendix C.3](appendix-c-adversarial-review.md)]** `RUSTFLAGS` is **global**: changing it evicts every crate in the workspace and dependency graph, including build scripts. As an environment variable it **clobbers** `[build] rustflags` in `.cargo/config.toml` rather than merging - silently discarding a user's sanitizer, target, and link flags.
- **[Fact - tested]** Option 2: with `--target-dir target/instrumented`, the wrapper was invoked for **every** crate (no stale artifacts exist in a fresh directory) and the user's default `target/` and flags were untouched.

#### Decision

**Build into an isolated `--target-dir` (default `target/instrumented`) whenever instrumentation is active. Do not touch `RUSTFLAGS`.** This must ship in the tool's first commit - everything built before it exists silently produces uninstrumented binaries.

#### Consequences

- ✅ Solves cache isolation **and** the stale-artifact problem together.
- ✅ The user's default `target/`, `RUSTFLAGS`, and `.cargo/config.toml` are untouched.
- ✅ Toggling instrumentation on and off never invalidates the other configuration's cache - both stay warm.
- ❌ **No cache sharing between instrumented and plain builds.** The first instrumented build compiles the whole graph from scratch, and disk usage roughly doubles.
- ❌ Rule-set changes still do not invalidate *within* the instrumented directory. A rule-set hash in the directory name, or a wrapper-side content check, is needed - and [§15.5](15-final-recommendation.md) Q1's CI regression test (toggle the ruleset, assert a rebuild) is what catches its absence.

#### Revisit if

- Cargo adds the wrapper to its fingerprint - then neither mechanism is needed. Re-run the [Appendix B](appendix-b-verification-log.md) item 2 experiment on each new toolchain; this is exactly the kind of internal behaviour that changes without a breaking-change note.
- The permanent loss of cache sharing makes rebuild times intolerable in practice ([§15.6](15-final-recommendation.md) names this as an abandon signal).

---

### ADR-005 - The eBPF branch is closed

**Status:** Accepted, 2026-09-04. Retires Architectures C and D and executes the [§15.6](15-final-recommendation.md) pivot condition.

#### Context

The project was framed as compile-time instrumentation **plus** a research branch: compiler-emitted metadata feeding an eBPF loader, resting on hypothesis **H2** - *logical async spans can be reconstructed at runtime from poll-level uprobe events plus compile-time state-machine metadata* ([§7.5](07-ebpf-future.md)). H2 was the project's most interesting-sounding claim and its least validated one.

#### Options considered

1. **Build the eBPF branch** - loader, metadata format, async reconstruction.
2. **Keep it deferred and gated** on an H2 experiment (the position before this round).
3. **Close it and contribute upstream instead.** Chosen.

#### Evidence

- **[Fact - [Appendix C.4](appendix-c-adversarial-review.md)]** The *mechanism* was never novel. USDT has embedded probe metadata in ELF notes since 2004, is consumed natively by `bpftrace`/`libbpf`/Aya, and two mature stable-Rust crates already emit it (`oxidecomputer/usdt` ~3.8M downloads, `cuviper/probe` ~1.8M). This killed the bespoke JSON sidecar and narrowed the claim from "compiler metadata for eBPF" to "**async-structure** metadata for eBPF."
- **[Fact - [Appendix D.4](appendix-d-maintainer-qa.md), OBI maintainer Giuseppe Ognibene (`#otel-ebpf`)]** A **working prototype** of Tokio async task reconstruction and context propagation in eBPF exists, tracked under **OBI issue #1096**, in final testing against task migration across worker threads and pointer reuse after free - the two edge cases this document independently predicted would be hardest ([Appendix C.9](appendix-c-adversarial-review.md) Q3).
- **[Fact - OBI maintainer Nikola Grcevski]** OBI today has **zero** application-level uprobes for Rust; it falls back to generic socket kprobes. #1096 is the work that changes that, and it is happening in the canonical upstream project.

[§15.6](15-final-recommendation.md) pre-committed to abandoning the branch if *"OBI ships semantic Rust function-level instrumentation upstream."* That condition fired in its strongest form: not a competitor shipping something adjacent, but the canonical project building the exact capability, with a reachable maintainer.

#### Decision

**Close the eBPF branch. Build no loader, no sidecar, no metadata artifact, and no competing async-reconstruction implementation, in any phase. Architectures C and D are abandoned and retained only as a research record. Contribute to #1096 instead.**

#### Consequences

- ✅ **H2 is answered before a phase was spent on it** - the best available outcome for a research question.
- ✅ **The project's scope becomes complementary rather than overlapping.** Compile-time instrumentation serves what eBPF structurally cannot reach: **macOS, Windows, unprivileged containers, non-root deployments** - regardless of how well #1096 works. That is a durable division of labour, not a consolation prize.
- ✅ [R19, R20, R21](13-technical-risks.md) close; [§15.5](15-final-recommendation.md) Q8/Q9 close; [Appendix C.9](appendix-c-adversarial-review.md) Q3/Q4/Q5 close.
- ✅ **Zero structural rework**, because [§11.1](11-recommended-architecture.md) chose Architecture A specifically so it would stand alone. The pre-written "do not abandon merely because the eBPF branch dies" bullet held.
- ❌ **The project loses its research-contribution claim.** What remains is an engineering contribution - a real one, and the one that was always missing ([§15.7](15-final-recommendation.md)).
- ❌ Positioning now depends on cross-platform support that **has never been tested** - every experiment ran on Windows ([R24](13-technical-risks.md)). The claim must be earned before it is made.

#### Revisit if

- #1096 is abandoned upstream **and** no successor appears. Even then, reopening requires re-validating H2 from scratch; a stalled prototype is not evidence the problem is tractable for us.
- A user need appears that eBPF genuinely cannot serve *and* compile-time instrumentation cannot either. None is currently known.
- **Not** a revisit condition: eBPF becoming interesting again. It is interesting; that was never the question.

---

### ADR-006 - The emitter is a seam

**Status:** Accepted, held since [§11.4](11-recommended-architecture.md), and vindicated by [ADR-001](#adr-001--generate-native-opentelemetry-api-calls).

#### Context

A rule says *what* to instrument. Something must decide *what code that becomes*. Fusing those two concerns is the natural, smaller design - and would have made [ADR-001](#adr-001--generate-native-opentelemetry-api-calls) a rewrite.

#### Options considered

1. **Fuse rule matching and code generation.** Smaller and simpler.
2. **Separate them behind an emitter interface.** Chosen - deliberate speculative generality, and the only piece of it this document endorses.

#### Evidence

Prospective when taken; **retrospective now**. [Appendix D.2](appendix-d-maintainer-qa.md) reversed the emitter decision after the documents had committed to `tracing` throughout. Because the seam was the plan, that reversal was an emitter swap plus a documentation pass. **The exact class of event the seam insured against occurred within one review round of the seam being proposed.**

#### Decision

**A rule expresses intent - "create a span here, named X, kind K, with attributes Y." A pluggable emitter decides the generated code. The default emitter is the native OpenTelemetry API.**

Emitters in scope:

| Emitter | Purpose |
| --- | --- |
| **Native OTel** | **Default.** `tracer.start(...)`, `FutureExt::with_context` for async ([ADR-001](#adr-001--generate-native-opentelemetry-api-calls)) |
| **C trampoline** | Tier 2 - dependency crates that cannot name `opentelemetry` ([§16.3](16-instrumentation-semantics.md)) |
| **`tracing`** | For users who want generated spans inside their existing `tracing` tree |
| **Dry-run** | Emits nothing; records what it would have done. Backs `--plan-only` and makes rule matching unit-testable without compiling |

#### Consequences

- ✅ Absorbed a full emitter reversal at zero structural cost.
- ✅ `fastrace` ([Appendix C.7](appendix-c-adversarial-review.md)) stays reachable if per-span cost is ever measured to be a problem.
- ✅ The dry-run emitter makes the rule engine testable in isolation - the cheapest test surface in the project.
- ✅ The Tier-1/Tier-2 split ([§16.3](16-instrumentation-semantics.md)) lands as two emitters rather than a fork in the splicer.
- ❌ One interface to design and keep honest. **The risk is emitter drift** - two emitters producing different span semantics. [§16](16-instrumentation-semantics.md) exists partly to prevent that: the semantics are specified once, and every emitter is verified against the same oracle table.

#### Revisit if

- After Phase 2, only one emitter has ever existed and no user has asked for another. Then the seam is unpaid-for generality and should be collapsed. *(It has already paid for itself once, so this is unlikely - but the condition is stated so the decision stays falsifiable.)*

---

---

← [Instrumentation Semantics](16-instrumentation-semantics.md) · [Contents](../../README.md) · [Appendix A - Sources](appendix-a-sources.md) →
