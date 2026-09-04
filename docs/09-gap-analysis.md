← [Competitive / Adjacent Landscape](08-competitive-landscape.md) · [Contents](../README.md) · [Architecture Candidates](10-architecture-candidates.md) →

---

## 9. Gap Analysis


### 9.1 What already exists?

- **A complete OTel export path.** `opentelemetry` + `opentelemetry_sdk` + `opentelemetry-otlp`, ~255M downloads, actively released. Metrics and logs stable. **[Fact]**
- **A complete, ecosystem-standard span model.** `tracing`, ~818M downloads. **[Fact]**
- **A mature tracing↔OTel bridge.** `tracing-opentelemetry` 0.33.0, ~196M downloads. **[Fact]**
- **Per-function compile-time instrumentation, opt-in.** `#[tracing::instrument]`, including correct async handling. **[Fact]**
- **A stable, documented build-interception hook.** `RUSTC_WRAPPER` / `RUSTC_WORKSPACE_WRAPPER`. **[Fact]**
- **Proven compiler-level instrumentation infrastructure.** MIR query overriding via custom drivers, with `rustc_plugin` as ready-made scaffolding, plus rustc source that explicitly accommodates it. **[Fact]**
- **Zero-code runtime instrumentation for Rust, at network granularity.** OBI. **[Fact]**
- **A reference design for exactly this problem in another language.** `otelc`, at v1.1. **[Fact]**

### 9.2 What partially exists?

- **Batch application of `#[instrument]`.** `tracing-orchestra` does it per module/impl block, is opt-in, first-party-only, and has been dormant since 2023. **[Fact]**
- **Automatic Rust instrumentation via eBPF.** OBI does it at network level; J00MZ attempts function level and is pre-release. **[Fact]**
- **Compiler-inserted function entry/exit hooks.** `-Z instrument-xray` exists but is nightly, semantically blind, and emits XRay sleds rather than telemetry. **[Fact]**
- **OTel-native instrumentation macros.** `otel-instrument` exists and is tiny. **[Fact]**

### 9.3 What appears to be missing?

1. **A tool that instruments a whole Rust crate graph, including third-party dependencies, at build time, without source edits.** Nothing found. **[Fact — negative result, see §4.6 caveats]**
2. **A declarative rule format for Rust instrumentation** (the analogue of `*.otelc.yml`). Nothing found.
3. **Third-party-distributable Rust instrumentation packages** (the analogue of `otelc`'s import-driven instrumentation crates). Nothing found.
4. ~~**Compiler-emitted *async state-machine* metadata for Rust.**~~ **NO LONGER OURS TO FILL — [Appendix D.4](appendix-d-maintainer-qa.md).** The mechanism was never missing (USDT, [Appendix C.4](appendix-c-adversarial-review.md)); the narrowed claim was the `.await` ↔ coroutine-state-variant content. That gap may still exist in the abstract, but it exists *in service of* eBPF async reconstruction, which OBI is now building (#1096). We do not fill it.
5. ~~**Semantically-aware async span reconstruction from below the source level.**~~ **BEING FILLED UPSTREAM — [Appendix D.4](appendix-d-maintainer-qa.md).** An OBI maintainer has a working Tokio prototype (#1096). "Nothing found, possibly not tractable" was accurate when written and is now out of date.

**[Inference]** Gaps 1–3 are the project. They are all compile-time, all stable-Rust, and none of them depends on a hypothesis.

### 9.4 Is the project actually differentiated?

**Partially, and it is important to be precise about which part.**

| Claim | Verdict |
| --- | --- |
| "Compile-time auto-instrumentation is a novel idea" | ✗ **No.** `otelc` is the OTel-official implementation of it for Go, at v1.1 |
| "AST rewriting behind a build hook is a novel mechanism" | ✗ **No.** That is precisely what `otelc` does |
| "Doing this for Rust is novel" | ✓ **Yes**, as far as we can determine. Nobody has done it, and Rust is absent from OTel's zero-code list |
| "The Rust-specific problems are novel" | ✓ **Partially.** Async/coroutine instrumentation semantics, monomorphization, and macro invisibility have no Go analogue. The async problem in particular has a genuinely different shape |
| ~~"Compiler-generated metadata for eBPF is novel"~~ | **WITHDRAWN — [Appendix D.4](appendix-d-maintainer-qa.md).** Not novel as a mechanism (USDT, 2004), and the narrowed "async state-machine content" claim is moot now that OBI #1096 is building the capability it would have served |
| ~~"Compile-time + eBPF + OTel combined is novel"~~ | **WITHDRAWN.** We are not combining them |
| "Serving the platforms eBPF cannot reach" | ✓ **Not novel, but durable** — and this is the better claim. macOS, Windows, unprivileged containers, and non-root deployments are unreachable by any eBPF approach, however good #1096 turns out to be. That is a structural division of labour, not a race |

**[Inference — the framing is now simpler than it was.]** This was described as *"a porting-and-adaptation project with one genuinely novel research question attached."* The research question has been answered upstream ([Appendix D.4](appendix-d-maintainer-qa.md)), so what remains is the porting-and-adaptation project: worthwhile, useful, clearly missing, and no longer carrying a speculative half that the previous sentence had to warn against depending on. Losing the research question costs the project its most interesting-sounding claim and none of its value — which is exactly what §11.2 chose Architecture A to guarantee.

### 9.5 What would make it merely a wrapper?

The project degenerates into a wrapper if:

- It only instruments functions in the user's own crate. `tracing-orchestra` already does that, and a `sed` script nearly does.
- It only provides a nicer SDK + OTLP setup helper. Half a dozen crates do that.
- It requires manual per-function opt-in. Then it is `#[instrument]` with extra steps.
- It hardcodes a fixed instrumentation set into the binary with no rule format. Then it is unextendable and dies when the first user wants `sqlx`.
- It cannot instrument dependencies. **This is the single test that separates a real tool from a wrapper**, because dependency coverage is the only thing a user cannot achieve themselves with an afternoon and a text editor.

### 9.6 What would make it technically meaningful?

- **Dependency-graph coverage.** Instrumenting `hyper`/`sqlx`/`tonic` in the build without touching them. Non-trivial, genuinely useful, currently impossible in Rust.
- **A rule language with version-aware matching.** So instrumentation survives dependency upgrades and can be shipped by third parties.
- **Correct async span semantics, demonstrated with tests.** Not "we wrap in `with_context`," but a test suite showing span durations, context attachment, and nesting are right across `.await`, `spawn`, worker-thread migration, and concurrent tasks.
- **Measured overhead.** Real numbers for build time, binary size, and runtime cost. **[Sharpened, [Appendix D.3](appendix-d-maintainer-qa.md)]** `otelc` publishes compile-time benchmarks (+275% single-package, +54% multi-package) but still **no application runtime-latency numbers**. Publishing measured *runtime* overhead for Rust auto-instrumentation would be a genuine first, not a formality.
- ~~**An async-structure metadata artifact with a specified format.**~~ **Dropped — [Appendix D.4](appendix-d-maintainer-qa.md).** It was "the bridge to the research half," and the research half is closed.
- ~~**Answering H2 experimentally.**~~ **Answered upstream — [Appendix D.4](appendix-d-maintainer-qa.md).** OBI #1096. The useful contribution here is now *reviewing* that work, and offering the §6.3/§7 analysis of Rust async structure to it.
- **[New] Working on macOS and Windows.** Trivial for us and impossible for eBPF — and therefore, post-[Appendix D.4](appendix-d-maintainer-qa.md), the clearest statement of what this tool is for.

### 9.7 Ranking the seven candidate directions

Scores are 1–5 (5 best). "Novelty" scores the direction's contribution *given* everything in §8.

| # | Direction | Feasibility | Novelty | Usefulness | Impl. complexity (5 = simplest) | Ecosystem compat. | Maintainability | Portfolio/research value | **Total** |
| --- | --- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| 2 | **Cargo/build-time instrumentation** (`RUSTC_WRAPPER` orchestration, dependency-graph analysis, rule engine) | 5 | 4 | 5 | 4 | 5 | 5 | 4 | **32** |
| 1 | **Source-level Rust transformation** (`syn`/CST rewriting) | 5 | 3 | 4 | 4 | 5 | 4 | 3 | **28** |
| 7 | **Combination: compile-time + eBPF + OTel** | 2 | 5 | 4 | 1 | 3 | 2 | 5 | **22** |
| 5 | **Compiler-generated metadata** | 3 | 5 | 3 | 3 | 4 | 2 | 5 | **25** |
| 3 | **rustc/MIR instrumentation** | 3 | 4 | 3 | 2 | 2 | 1 | 5 | **20** |
| 6 | **Compiler-assisted eBPF instrumentation** | 2 | 5 | 3 | 1 | 3 | 2 | 5 | **21** |
| 4 | **LLVM-level instrumentation** | 3 | 2 | 2 | 2 | 3 | 2 | 2 | **16** |

Notes on the scores:

- **(2) and (1) are one architecture in practice.** Direction 1 is the transformation; direction 2 is the delivery vehicle. Neither is useful alone: source rewriting without build integration cannot reach dependencies; build integration without a transformation has nothing to do. Their combination is Architecture A.
- **(5) scores high on novelty and research value but only medium on feasibility** because emitting metadata still requires compiler access (nightly) unless it is derived from source analysis — in which case it is a much weaker artifact.
- **(3) MIR** scores lowest on maintainability for the reasons in §3.5. Its feasibility is real; its shippability is not.
- **(4) LLVM** is dominated: it is harder than source, less semantic than MIR, and largely duplicates `-Z instrument-xray`.
- **(6) and (7)** carry the highest research value and the lowest feasibility, and both depend on H2.

**[Superseded — [Appendix D.4](appendix-d-maintainer-qa.md).]** Directions **5, 6, and 7 are withdrawn**: all three scored their novelty and research value on H2, which is now being answered upstream by OBI #1096. Their scores were not wrong — the ranking correctly put them below (2) and (1) on feasibility, and the project correctly picked the top of the table. **The live ranking is (2) + (1) — Architecture A — with (3) surviving only as a Phase 3 nightly spike.** Note what this does to the table's own logic: the two highest-scoring directions were never the ones that depended on the hypothesis, so removing the hypothesis changes the plan not at all.

---

---

← [Competitive / Adjacent Landscape](08-competitive-landscape.md) · [Contents](../README.md) · [Architecture Candidates](10-architecture-candidates.md) →
