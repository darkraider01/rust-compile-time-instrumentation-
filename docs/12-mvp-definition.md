← [Recommended Architecture](11-recommended-architecture.md) · [Contents](../README.md) · [Technical Risks](13-technical-risks.md) →

---

## 12. Phase 1 MVP Definition


### 12.1 Goal

```
ordinary Rust application (async, axum or plain tokio)
        ↓
  cargo instrument -- run
        ↓
automatic #[instrument]-equivalent injection into the user's crate
        ↓
tracing spans → tracing-opentelemetry → opentelemetry_sdk
        ↓
OTLP → collector → visible trace with correct nesting and durations
```

**Deliberate MVP restriction:** Phase 1 instruments **workspace crates only** (via `RUSTC_WORKSPACE_WRAPPER`). Dependency-graph coverage — the actual differentiator — is **Phase 2**, using the same machinery switched to `RUSTC_WRAPPER`.

**[Inference]** This looks like it concedes the whole value proposition, and it is worth being explicit about why it does not. Phase 1's job is to prove the *pipeline* — Cargo hook → source rewrite → stock compile → correct spans → OTLP — on code we control, where source layout is simple and failures are debuggable. Rewriting third-party crates adds a large, independent set of problems (read-only registry sources, `build.rs`-generated modules, `include!`, exotic `cfg`) that would obscure whether the core pipeline works. The wrapper mechanism is the same; only the scope changes.

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
| Third-party dependencies | **Phase 2** |
| Cross-process context propagation | Phase 2+ |
| Argument value capture | Off; `skip_all` always in Phase 1 |
| `no_std` crates | Out of scope |
| Metrics, logs | Out of scope |
| eBPF | **Out of scope, hard boundary** |

### 12.4 Required toolchain and dependencies

- **Toolchain:** stable Rust. Target the current stable minus three (matching `opentelemetry-rust`'s own support window **[Fact]**). Nightly must not be required for anything.
- **Platform:** Linux/macOS/Windows for the tool itself. No kernel requirements — this is one of the advantages of not doing eBPF.

Tool dependencies: `syn` (full features), `quote`, `proc-macro2`, `prettyplease`, `serde` + `serde_yaml`, `cargo_metadata`, `clap`, `anyhow`/`thiserror`, `tracing` (for the tool's own diagnostics). **[Fact — confirmed by a hands-on round-trip experiment during verification, see [Appendix B](appendix-b-verification-log.md) item 7]** `syn` 2 + `prettyplease` 0.2 silently drop every non-doc `//` comment and fully reformat the entire touched file to `prettyplease`'s canonical style (doc comments, attributes, and all structural content survive intact, and the round-trip is idempotent). This is now a known, quantified limitation rather than an open question — see §12.9 O2 and R11.

Injected/runtime dependencies (added to the user's project): `tracing`, `tracing-subscriber`, `tracing-opentelemetry`, `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`.

### 12.5 CLI

```
cargo instrument [OPTIONS] -- <cargo args...>

  --rules <PATH>            Additional rule file(s). Repeatable.
  --plan-only               Run analysis, print the plan, do not build.
  --emit-plan <PATH>        Write the plan as JSON (the metadata-artifact seed).
  --dump-rewritten <DIR>    Write rewritten sources here for inspection.
  --level <LEVEL>           Span level for generated instrumentation (default: debug).
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
          level: debug
          name: "{{.CratePath}}::{{.FnName}}"
          skip_all: true
          record_error: true
```

Only one rule type (`inject_span`, our `inject_hooks` analogue) in Phase 1. `wrap_call` — needed for `tokio::spawn` propagation — is the first addition.

### 12.7 Success criteria

The MVP is done when all of these hold:

1. **It builds a real async application unmodified.** A small axum + tokio + `sqlx`-or-mock service compiles and runs under `cargo instrument` with no source edits beyond adding the tool.
2. **Spans appear in a collector.** Traces reach an OTel Collector over OTLP and render correctly in Jaeger or equivalent.
3. **Nesting is correct.** A synchronous call chain `a → b → c` produces three correctly nested spans.
4. **Async durations are correct.** An `async fn` that awaits a 100 ms sleep produces a span whose *total* duration is ≈100 ms and whose *busy* time is ≈0. This is the single most important correctness test in the MVP.
5. **Concurrency does not corrupt spans.** Ten concurrently spawned tasks each running an instrumented async function produce ten independent, non-interleaved span trees.
6. **Nothing that should be skipped is instrumented.** Every entry in §12.3 is verified skipped by a test.
7. **The build does not break.** The tool runs successfully over a corpus of at least five real open-source Rust crates in `--plan-only` mode and at least two in full build mode without producing a compile error.
8. **Overhead is measured and published.** Build time, binary size, and a runtime microbenchmark, each with an uninstrumented baseline. Numbers, not adjectives.
9. **`--plan-only` output is comprehensible.** A human can read it and predict what will be instrumented.

### 12.8 Tests required

**Unit — rule engine**
- Rule parsing, including malformed rules producing useful errors.
- Matcher selectivity: crate glob, `is_async`, `not` combinators.
- Precedence: `INSTRUMENT_RULES` > `--rules` > project file > defaults.

**Unit — AST transformation**
- Snapshot tests (`insta`) of rewritten output for: free fn, method, trait impl method, generic fn, `async fn`, `Result`-returning fn.
- Idempotence: a function already carrying `#[instrument]` is not double-instrumented. **This is the duplicate-instrumentation guard and must exist before the first real build.**
- Round-trip fidelity: parse → print of an untouched file compiles identically.

**Integration — compilation**
- Every §12.2 construct compiles after instrumentation.
- Every §12.3 construct is skipped and the crate still compiles.
- Instrumented and uninstrumented builds produce the same program output.

**Integration — telemetry correctness** (in-process collector, no network)
- Sync nesting (criterion 3).
- Async duration: total ≈ 100 ms, busy ≈ 0 (criterion 4).
- Concurrent task isolation (criterion 5).
- `Err` return recorded on the span.
- Panic: span still closes (under `panic=unwind`).
- Span attributes contain function name, module path, file, and line.

**Regression / corpus**
- `--plan-only` over ≥5 real crates, snapshotted, so rule changes show their blast radius.
- Full instrumented build of ≥2 real crates in CI.

**Benchmarks** (reported, not asserted — no pass/fail thresholds in CI)
- Clean and incremental build time, instrumented vs. not.
- Binary size delta.
- Runtime: instrumented + exporter, instrumented + `STATIC_MAX_LEVEL` off, uninstrumented.

### 12.9 Open questions to resolve during Phase 1

| # | Question | Why it matters | How to resolve |
| --- | --- | --- | --- |
| O1 | ~~Does `RUSTC_WRAPPER` participate in Cargo's fingerprint so instrumented and plain builds cache separately?~~ **RESOLVED — [Fact, confirmed by direct experiment]** | See [Appendix B](appendix-b-verification-log.md) item 2: on Cargo 1.97.1, it does **not**. Building a crate, then setting `RUSTC_WRAPPER` with no source change, produces zero recompilation — Cargo reports "Finished" with no "Compiling" line and never invokes the wrapper for the actual crate compile. `RUSTC_WORKSPACE_WRAPPER` behaves identically. | Confirmed mitigation, also experimentally validated: pair every wrapper toggle / rule-set change with a synthetic `RUSTFLAGS` value (e.g. a `--cfg` carrying a hash of the active rule set). `RUSTFLAGS` changes were confirmed to force recompilation on every change, three-for-three in testing. This mitigation must ship in the tool's first commit, not be added later. |
| O2 | `syn` + `prettyplease` or `ra_ap_syntax` CST? **Partially resolved** | **[Fact, confirmed by direct experiment]** See Appendix B item 7: `syn`+`prettyplease` round-tripping of a representative sample destroyed all 4 non-doc `//` comments in the sample while fully preserving doc comments, attributes, cfg, generics, async signatures, and macro bodies; it also reformatted the *entire* file to canonical style, not just the touched item, and did so idempotently (a second pass changed nothing further). This is a real, now-quantified cost, not a guess — every instrumented file's diff will look like a full reformat, and every non-doc comment in it will silently vanish. | Still open: whether `ra_ap_syntax`'s lossless CST avoids this at an acceptable API-stability cost. Prototype it and compare directly against the now-known `syn` baseline above. |
| O3 | Where do rewritten sources live, and how do relative paths (`include!`, `#[path]`, `mod` file resolution) survive? | Determines whether Phase 2 dependency rewriting is viable at all | Prototype on a crate using `include!` and `build.rs`-generated modules |
| O4 | Does `#[instrument]` compose correctly with `#[async_trait]`, `#[tokio::main]`, and common attribute macros, and in what order? | Determines a large slice of real-world compatibility | Compilation tests |
| O5 | How do we avoid double-instrumenting a crate that already uses `#[instrument]`? | Duplicate spans corrupt traces | Detect existing attributes in the AST; test |
| O6 | ~~Exact `code.*` semantic-convention attribute names and their stability~~ **RESOLVED** | **[Fact]** Stable since semconv v1.33.0: `code.function.name`, `code.file.path`, `code.line.number`, `code.column.number`, `code.stacktrace`. See [§5.3](05-otel-rust.md). | — |
| O7 | Does the tool need to modify `Cargo.toml` (to add `tracing` etc.), and how do we do that without wrecking the user's lockfile? | Phase 1 of `otelc` solves the analogue with `go mod tidy`; Cargo's feature unification makes this harder | Prototype; consider requiring the user to add dependencies manually in Phase 1 |

---

---

← [Recommended Architecture](11-recommended-architecture.md) · [Contents](../README.md) · [Technical Risks](13-technical-risks.md) →
