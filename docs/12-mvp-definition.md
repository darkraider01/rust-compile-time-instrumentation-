← [Recommended Architecture](11-recommended-architecture.md) · [Contents](../README.md) · [Technical Risks](13-technical-risks.md) →

---

## 12. Phase 1 MVP Definition


### 12.1 Goal

```
ordinary Rust application (async, axum or plain tokio)
   + exactly one third-party dependency
        ↓
  cargo instrument -- build
        ↓
automatic instrumentation injection into the user's crate AND that one dependency
   (extern "C" trampolines: __otel_span_enter / __otel_span_exit)
        ↓
native OpenTelemetry API calls → opentelemetry_sdk
   async sites wrapped via opentelemetry::trace::FutureExt::with_context
        ↓
OTLP → collector → visible trace with correct nesting and durations,
       including a span from inside the unmodified dependency
```

**[Revised after the adversarial review round — see [Appendix C](appendix-c-adversarial-review.md).** The original MVP scoped Phase 1 to workspace crates only, deferring dependency instrumentation to Phase 2 on the grounds that the cross-crate injection mechanism was unproven. It has since been proven — extern `"C"` trampolines compile and run cleanly against an undeclared third-party crate on stable Rust (Appendix C.2). Deferring the proven mechanism now would only defer the project's actual differentiator for no remaining technical reason. **The MVP now includes exactly one dependency**, kept small deliberately: enough to prove the mechanism generalizes past the workspace boundary, not enough to take on the harder problems (`build.rs`-generated modules, exotic `cfg`, macro-heavy crates) that real dependency-graph coverage will eventually require in Phase 2.]**

**[Revised again after the maintainer Q&A round — see [Appendix D.2](appendix-d-maintainer-qa.md).** The generated code is now native OpenTelemetry API calls, not `tracing` spans. This changes §12.4's dependency set and success criterion 4 below; it does not change the injection mechanism, the MVP's scope, or its one-dependency slice.]**

### 12.1a The dependency-coverage mechanism: `extern "C"` trampolines

**[Fact — confirmed by direct experiment, [Appendix C.2](appendix-c-adversarial-review.md).]** This is the mechanism the whole differentiator rests on, so it is stated here in full rather than left in an appendix.

The wrapper splices into a dependency's source **only a symbol declaration**, and appends **no flags at all** to the `rustc` argv:

```rust
unsafe extern "C" {
    fn __otel_span_enter(name: *const u8, len: usize);
    fn __otel_span_exit(/* handle */);
}
```

Instrumented call sites in that dependency call those symbols. Resolution happens at the **application's** final link step, from a runtime crate the application declares as an ordinary dependency.

Why this shape and not the alternatives:

| Property | Consequence |
| --- | --- |
| No `--extern`, no `-L`, no manifest edit | The dependency's `Cargo.toml` and the lockfile are provably untouched (§12.9 O7). Nothing enters Cargo's DAG |
| No crate-graph edge | Structurally cannot produce duplicate-crate or dependency-cycle errors — the failure class that sank Mechanism 1 ([Appendix C.2](appendix-c-adversarial-review.md)) |
| No metadata propagation | Downstream consumers of the instrumented crate need no flags either. Mechanism 1 required `-L` on **every** downstream crate (it failed with `E0463` until that was added) |
| C ABI | Stable across compiler versions and crate boundaries by definition |
| Mirrors `otelc`'s own design | The trampoline indirection in [§2.5](02-otelc-go.md), reached without Go's `//go:linkname` |

**The runtime is pre-built standalone in Phase 1**, outside Cargo's dependency graph, which is what defeats the topological-scheduling objection: Cargo never needs to know the runtime exists as a graph node ([§10](10-architecture-candidates.md), Architecture A).

**Untested edges** carried into Phase 1: `lto = true` + `codegen-units = 1` + `panic = "abort"` ([Appendix C.9](appendix-c-adversarial-review.md) Q7 — regression test specified in §12.8), and source-tree mirroring against real multi-file crates ([Appendix D.6](appendix-d-maintainer-qa.md) D-Q4, now the project's largest open unknown).

### 12.2 Supported constructs

| Construct | Supported |
| --- | --- |
| Free functions | ✅ |
| Inherent methods (`impl Foo`) | ✅ |
| Trait impl methods (`impl Trait for Foo`) | ✅ |
| Generic functions and methods | ✅ (one site per definition) |
| `async fn` (free, inherent, trait impl) | ✅ **required** |
| Functions returning `Result<_, _>` | ✅ (error recorded on the span) |
| `pub` and private items | ✅ |

### 12.3 Explicitly unsupported

| Construct | Behaviour |
| --- | --- |
| Closures, `async` blocks | Skipped silently |
| `const fn` | Skipped (would be a compile error) |
| `extern "C"` / `unsafe extern` | Skipped |
| Directly self-recursive functions | Skipped; reported in the plan |
| `#[inline]` / functions below the size threshold | Skipped |
| Macro-generated items | Not seen at all; documented |
| `std` / precompiled crates | Out of scope |
| Third-party dependencies | **One dependency in Phase 1** (see §12.1 revision), to prove the mechanism generalizes past the workspace boundary. Full dependency-graph coverage across an arbitrary crate graph is **Phase 2**. |
| Cross-process context propagation | Phase 2+ |
| Argument value capture | Off; `skip_all` always in Phase 1 |
| `no_std` crates | Out of scope |
| Metrics, logs | Out of scope |
| eBPF | **Out of scope, hard boundary** |

### 12.4 Required toolchain and dependencies

- **Toolchain:** stable Rust. Target the current stable minus three (matching `opentelemetry-rust`'s own support window **[Fact]**). Nightly must not be required for anything.
- **Platform:** Linux/macOS/Windows for the tool itself. No kernel requirements — this is one of the advantages of not doing eBPF.

Tool dependencies: `syn` (full features, used for **parsing/analysis only** — no `quote`/`prettyplease` in the injection path), `serde` + `serde_yaml`, `cargo_metadata`, `clap`, `anyhow`/`thiserror`, `tracing` (for the tool's own diagnostics). **[Revised after the adversarial review round, see [Appendix C.6](appendix-c-adversarial-review.md)]** The original design planned to parse with `syn` and re-emit with `prettyplease`, which a hands-on experiment confirmed silently drops every non-doc `//` comment and reformats the whole touched file (Appendix B item 7). Following `cargo-mutants`' proven approach instead: `syn` is used only to locate target items and read `span().byte_range()`; the actual code change is a **byte-range splice into the original UTF-8 source buffer**. This preserves comments, formatting, and line numbers exactly outside the insertion point, and removes the need to evaluate `ra_ap_syntax` as an alternative — see §12.9 O2.

Injected/runtime dependencies (added to the **application** crate only — never to an instrumented dependency, per §12.1a): `opentelemetry` (traces API, including `trace::FutureExt`), `opentelemetry_sdk`, `opentelemetry-otlp`, plus our own small runtime crate exporting the `__otel_span_*` symbols.

**[Revised after the maintainer Q&A round, see [Appendix D.2](appendix-d-maintainer-qa.md)]** `tracing`, `tracing-subscriber`, and `tracing-opentelemetry` are **no longer injected**. The original design generated `#[tracing::instrument]` on the premise that only `tracing` models async future interleaving correctly; OTel Rust maintainer Scott Gerring disproved that — `opentelemetry::trace::FutureExt::with_context` attaches the context on each `poll()` and detaches on yield, matching `tracing::Instrument`'s lifecycle. Dropping the three crates closes [R14](13-technical-risks.md) (the deliberate `tracing-opentelemetry` ↔ `opentelemetry` version offset) and removes the bridge's hot-path context synchronisation. A `tracing` emitter remains available behind the §11.4 seam for users who want generated spans inside their existing `tracing` tree.

**Generated forms:**

```rust
// sync fn — guard to end of scope
let _otel_guard = __otel_span_enter(/* name, kind, code.* attrs */);

// async fn — no guard held across .await; the future carries the context
async move { /* original body */ }.with_context(cx)
```

### 12.5 CLI

```
cargo instrument [OPTIONS] -- <cargo args...>

  --rules <PATH>            Additional rule file(s). Repeatable.
  --plan-only               Run analysis, print the plan, do not build.
  --emit-plan <PATH>        Write the plan as JSON (the metadata-artifact seed).
  --dump-rewritten <DIR>    Write rewritten sources here for inspection.
  --verbosity <TIER>        Generation-time selectivity tier (default: normal).
                            Chooses WHICH functions get spans; OTel spans have no
                            level field, so this is resolved at splice time, not
                            at runtime (Appendix D.2).
  --dry-run                 Report what would be instrumented; touch nothing.

Environment:
  INSTRUMENT_RULES          Highest-precedence rule source (mirrors OTELC_RULES).
  OTEL_EXPORTER_OTLP_ENDPOINT, OTEL_SERVICE_NAME   Standard OTel env vars.
```

Examples:

```
cargo instrument -- run
cargo instrument --plan-only -- build
cargo instrument --dump-rewritten ./target/instrumented -- build --release
```

**Design note:** `--plan-only`, `--dry-run`, and `--dump-rewritten` are not conveniences; they are the primary debugging affordances of the whole architecture and should exist from the first commit.

### 12.6 Rule format (Phase 1 subset)

Deliberately a strict subset of `otelc`'s schema, so growth is additive:

```yaml
rules:
  instrument_all_async:
    target: "crate:my_service"          # crate name, or glob
    where:
      is_async: true
      not:
        - is_const
        - is_extern
        - is_closure
    do:
      - inject_span:
          name: "{{.CratePath}}::{{.FnName}}"
          kind: internal              # OTel SpanKind: internal | server | client
          skip_args: true             # never capture argument values (R17)
          record_error: true
```

Only one rule type (`inject_span`, our `inject_hooks` analogue) in Phase 1. `wrap_call` — needed for `tokio::spawn` propagation — is the first addition.

**[Revised after the maintainer Q&A round, [Appendix D.2](appendix-d-maintainer-qa.md)]** `level` is gone (OTel spans have no level; generation-time selectivity is `--verbosity` and rule matching), `skip_all` became `skip_args`, and `kind` is new — span kinds are first-class in the native API and were only reachable through `tracing-opentelemetry`'s `otel.kind` magic field before. The rule vocabulary describes *intent*; the §11.4 emitter decides the code.

### 12.7 Success criteria

The MVP is done when all of these hold:

1. **It builds a real async application unmodified.** A small axum + tokio + `sqlx`-or-mock service compiles and runs under `cargo instrument` with no source edits beyond adding the tool.
1a. **The one MVP dependency is instrumented without any edit to its own source, `Cargo.toml`, or lockfile.** (Appendix C.2/C.8 item 5.) This is the criterion that actually validates the differentiator, not just the pipeline.
2. **Spans appear in a collector.** Traces reach an OTel Collector over OTLP and render correctly in Jaeger or equivalent — including at least one span produced from inside that dependency.
3. **Nesting is correct.** A synchronous call chain `a → b → c` produces three correctly nested spans.
4. **Async durations and context are correct.** **[Revised, [Appendix D.2](appendix-d-maintainer-qa.md)]** An `async fn` that awaits a 100 ms sleep produces exactly **one** span whose duration is ≈100 ms — not one span per `poll()`, and not a duration that collapses to the CPU-busy time. The `busy`/`idle` half of the original criterion is dropped: it was a `tracing-opentelemetry` synthesis with no field in the OTel data model. What replaces it, and is the sharper test of `with_context`: **while the task is suspended, the span's context is not current on the thread that was polling it** — a second, unrelated instrumented function running on that thread during the sleep must not become a child. This is the single most important correctness test in the MVP.
5. **Concurrency does not corrupt spans.** Ten concurrently spawned tasks each running an instrumented async function produce ten independent, non-interleaved span trees. **[Added]** Includes tasks migrating between Tokio worker threads mid-await ([Appendix D.6](appendix-d-maintainer-qa.md) D-Q1) — the case `with_context`'s attach-per-poll design exists to handle, and the one worth proving rather than assuming.
6. **Nothing that should be skipped is instrumented.** Every entry in §12.3 is verified skipped by a test.
7. **The build does not break.** The tool runs successfully over a corpus of at least five real open-source Rust crates in `--plan-only` mode and at least two in full build mode without producing a compile error.
8. **Overhead is measured and published.** Build time, binary size, and a runtime microbenchmark, each with an uninstrumented baseline. Numbers, not adjectives.
9. **`--plan-only` output is comprehensible.** A human can read it and predict what will be instrumented.

### 12.8 Tests required

**Unit — rule engine**
- Rule parsing, including malformed rules producing useful errors.
- Matcher selectivity: crate glob, `is_async`, `not` combinators.
- Precedence: `INSTRUMENT_RULES` > `--rules` > project file > defaults.

**Unit — source splicing** *(revised from "AST transformation" per [Appendix C.6](appendix-c-adversarial-review.md) — `syn` is analysis-only, the edit is a byte-range splice, not a parse→print round-trip)*
- Snapshot tests (`insta`) of spliced output for: free fn, method, trait impl method, generic fn, `async fn`, `Result`-returning fn.
- Idempotence: a function already instrumented — by us, or by hand with `#[instrument]` / an explicit OTel span — is not double-instrumented. **This is the duplicate-instrumentation guard and must exist before the first real build.**
- **Splice fidelity**: the output is byte-identical to the input everywhere outside the inserted region — comments, blank lines, and exact formatting all survive. This replaces the old "round-trip fidelity" test, which only checked that the file still compiled, not that it was left otherwise untouched.

**Integration — compilation**
- Every §12.2 construct compiles after instrumentation.
- Every §12.3 construct is skipped and the crate still compiles.
- Instrumented and uninstrumented builds produce the same program output.

**Integration — telemetry correctness** (in-process span exporter, no network)
- Sync nesting (criterion 3).
- Async duration: exactly one span, duration ≈ 100 ms (criterion 4).
- Context is not current on the polling thread while the task is suspended (criterion 4).
- Concurrent task isolation, including migration across worker threads (criterion 5).
- `Err` return recorded on the span.
- Panic: span still closes (under `panic=unwind`).
- Span attributes contain function name, module path, file, and line.

**Regression / corpus**
- `--plan-only` over ≥5 real crates, snapshotted, so rule changes show their blast radius.
- Full instrumented build of ≥2 real crates in CI.
- **[New, per Appendix C open question Q7]** At least one instrumented build with `lto = true`, `codegen-units = 1`, and `panic = "abort"` set — the trampoline-linking behaviour under these settings is untested and could fail silently at link time.

**Benchmarks** (reported, not asserted — no pass/fail thresholds in CI)
- Clean and incremental build time, instrumented vs. not. Expected range from real `otelc` data: **1.5×–3× clean compile**, worst on small projects ([Appendix D.3](appendix-d-maintainer-qa.md)).
- Binary size delta.
- Runtime, four configurations **[revised, [Appendix D.2](appendix-d-maintainer-qa.md)]**: uninstrumented; instrumented with the `--cfg` gate off (call sites deleted at compile time — should equal baseline); instrumented, gate on, SDK not recording (the cost that replaces `STATIC_MAX_LEVEL`, [R23](13-technical-risks.md) / D-Q3); instrumented, recording, exporting to a local collector.

### 12.9 Open questions to resolve during Phase 1

| # | Question | Why it matters | How to resolve |
| --- | --- | --- | --- |
| O1 | ~~Does `RUSTC_WRAPPER` participate in Cargo's fingerprint so instrumented and plain builds cache separately?~~ **RESOLVED — [Fact, confirmed by direct experiment]** | See [Appendix B](appendix-b-verification-log.md) item 2: on Cargo 1.97.1, it does **not**. Building a crate, then setting `RUSTC_WRAPPER` with no source change, produces zero recompilation — Cargo reports "Finished" with no "Compiling" line and never invokes the wrapper for the actual crate compile. `RUSTC_WORKSPACE_WRAPPER` behaves identically. | **[Revised — see [Appendix C.3](appendix-c-adversarial-review.md)]** The originally proposed `RUSTFLAGS`-hash mitigation works but is destructive: `RUSTFLAGS` is global, so changing it evicts the *entire* workspace and dependency cache (not just the instrumented crate), and setting it as an environment variable clobbers any `[build] rustflags` in `.cargo/config.toml` without merging. **Confirmed working replacement, experimentally validated:** build into an isolated `--target-dir` (e.g. `target/instrumented`) whenever the wrapper is active. This forces the wrapper to run for every crate in that directory (there is no stale cache to reuse there) while leaving the user's default `target/` and their `RUSTFLAGS`/config untouched. |
| O2 | ~~`syn` + `prettyplease` or `ra_ap_syntax` CST?~~ **RESOLVED — neither; adopt byte-range splicing** | **[Fact, confirmed by direct experiment]** See Appendix B item 7: `syn`+`prettyplease` round-tripping of a representative sample destroyed all 4 non-doc `//` comments in the sample while fully preserving doc comments, attributes, cfg, generics, async signatures, and macro bodies; it also reformatted the *entire* file to canonical style, not just the touched item. | **[Resolved in Appendix C.6]** `cargo-mutants` solves exactly this problem by using `syn` for analysis only and applying mutations **textually** via byte spans, so untouched code keeps its original formatting, comments, and line numbers. Adopting the same technique removes the `prettyplease`-vs-`ra_ap_syntax` trade-off entirely — neither is needed in the injection path. |
| O3 | Where do rewritten sources live, and how do relative paths (`include!`, `#[path]`, `mod` file resolution) survive? | Determines whether Phase 2 dependency rewriting is viable at all | Prototype on a crate using `include!` and `build.rs`-generated modules |
| O4 | **[Reframed, [Appendix D.2](appendix-d-maintainer-qa.md)]** ~~Does `#[instrument]` compose with `#[async_trait]`/`#[tokio::main]`?~~ Does a **body splice** survive functions that attribute macros rewrite — `#[async_trait]` (desugars the body into a boxed future), `#[tokio::main]`, `#[test]`? | Determines a large slice of real-world compatibility. Note this is a *different and probably easier* question than the attribute-ordering one: we insert statements into a body rather than adding an attribute whose expansion order matters | Compilation tests across the macro matrix |
| O5 | How do we avoid double-instrumenting a function that is already instrumented, by `#[instrument]` or by a hand-written OTel span? | Duplicate spans corrupt traces | Detect existing attributes and existing `tracer.start`/`with_context` calls in the AST; test |
| O6 | ~~Exact `code.*` semantic-convention attribute names and their stability~~ **RESOLVED** | **[Fact]** Stable since semconv v1.33.0: `code.function.name`, `code.file.path`, `code.line.number`, `code.column.number`, `code.stacktrace`. See [§5.3](05-otel-rust.md). | — |
| O7 | ~~Does the tool need to modify `Cargo.toml` (to add `tracing` etc.), and how do we do that without wrecking the user's lockfile?~~ **Substantially narrowed** | **[Revised — see [Appendix C.2](appendix-c-adversarial-review.md)]** With the extern `"C"` trampoline mechanism, an **instrumented dependency needs no `Cargo.toml` change at all** — the wrapper injects only a symbol declaration, resolved at the application's own final link step. Only the **application crate** needs `tracing`/`tracing-opentelemetry`/`opentelemetry-otlp` (which it needs anyway, as the runtime owner) — a normal, first-party `Cargo.toml` edit, not a synthetic edit to a dependency's manifest. | Confirm in the MVP's dependency slice (§12.1): the instrumented dependency's own `Cargo.toml`/lockfile should be provably untouched after a build. |

---

---

← [Recommended Architecture](11-recommended-architecture.md) · [Contents](../README.md) · [Technical Risks](13-technical-risks.md) →
