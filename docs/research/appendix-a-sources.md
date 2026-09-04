← [Architecture Decision Records](17-decision-records.md) · [Contents](../../README.md) · [Appendix B - Verification Log](appendix-b-verification-log.md) →

---

## Appendix A - Primary sources consulted

Sources from the original research pass (2026-09-04), the subsequent verification pass (2026-09-04, same day - see [Appendix B](appendix-b-verification-log.md) for what each new source resolved), the adversarial review round ([Appendix C](appendix-c-adversarial-review.md)), and the maintainer Q&A round ([Appendix D](appendix-d-maintainer-qa.md)).

### Evidence provenance - four tiers, not one

Added in the Phase 0 completion audit, because the list below mixes grades of evidence that a reader should not have to disentangle by inspection. In descending order of how independently checkable a claim is:

| Tier | What it is | Reader can re-check? | Where |
| --- | --- | --- | --- |
| **1 - Published primary source** | Documentation, source code, issue threads, repository metadata | **Yes**, at the URL | Most of this appendix |
| **2 - Hands-on experiment** | Code written and run on the local toolchain | **In principle** - the protocol is documented, the code is not published | [Appendix E.1](appendix-e-experiment-matrix.md) (6 experiments) |
| **3 - Maintainer testimony** | Direct answers in the OpenTelemetry community Slack | **No** - not a published document; attributed by name and SIG instead | [Appendix D](appendix-d-maintainer-qa.md) |
| **4 - Unmerged upstream work** | OBI #1096's prototype, described by its author | **No, and not yet** - the code is unmerged | [Appendix D.4](appendix-d-maintainer-qa.md) |

**[Inference]** Tier 3 and Tier 4 carry two of this project's biggest decisions ([ADR-001](17-decision-records.md) and [ADR-005](17-decision-records.md)). That is defensible - the people who maintain the code are the best available source on what it does and what they are building - but it is worth stating plainly, because both are unverifiable by a reader working only from this repository. **ADR-001 has an independent Tier-1 backstop** (`FutureExt::with_context` is documented API, so the maintainer's answer can be checked against the crate). **ADR-005 does not** - if OBI #1096 stalls or never merges, the decision rests on testimony alone, which is why [ADR-005](17-decision-records.md)'s revisit condition names exactly that scenario.

**OpenTelemetry Go compile-time instrumentation**
- https://github.com/open-telemetry/opentelemetry-go-compile-instrumentation
- https://github.com/open-telemetry/opentelemetry-go-compile-instrumentation/blob/main/docs/rules.md
- https://github.com/open-telemetry/opentelemetry-go-compile-instrumentation/blob/main/docs/implementation.md
- https://github.com/open-telemetry/opentelemetry-go-compile-instrumentation/blob/main/docs/getting-started.md
- https://github.com/open-telemetry/opentelemetry-go-compile-instrumentation/blob/main/docs/adr/0005-import-driven-instrumentation-selection.md
- https://github.com/open-telemetry/opentelemetry-go-compile-instrumentation/blob/main/docs/benchmarking.md (read in full during verification - confirmed to contain no runtime/binary-size numbers, only a CI compile-time-overhead gate)
- https://opentelemetry.io/docs/zero-code/go/compile-time/
- https://opentelemetry.io/blog/2026/go-compile-time-instrumentation-v1/
- Release metadata via the GitHub releases API (v1.0.0 2026-07-14, v1.1.0 2026-08-24)
- https://github.com/open-telemetry/opentelemetry-go-instrumentation (the official Go eBPF auto-instrumentation project - confirmed to exist; distinct from `otelc`, and the explicit "inspiration" cited by J00MZ/opentelemetry-rust-instrumentation)

**OpenTelemetry general**
- https://opentelemetry.io/docs/zero-code/
- https://opentelemetry.io/docs/zero-code/obi/
- https://opentelemetry.io/docs/zero-code/obi/distributed-traces/
- https://opentelemetry.io/docs/languages/rust/
- https://opentelemetry.io/docs/specs/semconv/registry/attributes/code/ (verification: confirmed `code.function.name`/`code.file.path`/`code.line.number`/`code.column.number`/`code.stacktrace` are Stable)
- https://opentelemetry.io/docs/specs/semconv/non-normative/code-attrs-migration/
- https://github.com/open-telemetry/community/issues/2406 (Beyla donation)
- https://github.com/open-telemetry/community/issues/252 (Rust SIG kickoff)
- https://grafana.com/docs/beyla/latest/distributed-traces/
- https://grafana.com/docs/beyla/latest/ (verification: language compatibility framing, "validate non-Go async/reactive trace support before production" caution)
- https://deepwiki.com/open-telemetry/opentelemetry-ebpf-instrumentation/3-architecture-overview (verification: Go Tracer vs. Generic Tracer split - confirms Rust receives kprobe/socket-filter treatment only, no application-level uprobes)

**OpenTelemetry Rust / tracing**
- https://github.com/open-telemetry/opentelemetry-rust
- https://github.com/open-telemetry/opentelemetry-rust/issues/1571 (re-read in full during verification via the GitHub API: confirmed **closed** 2026-03-18, resolved as "maintain both APIs")
- https://github.com/open-telemetry/opentelemetry-rust/issues/1690 (the "context synchronisation issue" tracked separately from #1571)
- https://github.com/open-telemetry/opentelemetry-rust/pull/3122 (verification: merged PR adding `docs/traces.md`/`docs/logs.md`)
- https://raw.githubusercontent.com/open-telemetry/opentelemetry-rust/main/docs/traces.md (verification: current official guidance - "For new code, prefer the OpenTelemetry Tracing API directly")
- https://github.com/tokio-rs/tracing
- https://github.com/tokio-rs/tracing-opentelemetry
- https://docs.rs/tracing-attributes/latest/tracing_attributes/attr.instrument.html
- https://docs.rs/tracing/latest/tracing/level_filters/index.html
- crates.io API metadata for `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `tracing`, `tracing-subscriber`, `tracing-opentelemetry`, `autometrics`, `otel-instrument`, `tracing-orchestra`, `instrument-level`

**Rust compiler**
- https://rustc-dev-guide.rust-lang.org/overview.html
- https://rustc-dev-guide.rust-lang.org/rustc-driver/remarks-on-perma-unstable-features.html
- https://github.com/rust-lang/rust/blob/master/compiler/rustc_mir_transform/src/lib.rs (read directly; MIR pass ordering and the `#114628` custom-driver comment)
- https://doc.rust-lang.org/nightly/nightly-rustc/rustc_mir_transform/coroutine/index.html
- https://doc.rust-lang.org/stable/unstable-book/language-features/rustc-private.html
- https://doc.rust-lang.org/rustc/symbol-mangling/index.html
- https://doc.rust-lang.org/rustc/instrument-coverage.html
- https://doc.rust-lang.org/cargo/reference/config.html
- https://github.com/rust-lang/rust/pull/90132 (`-C instrument-coverage` stabilisation)
- https://github.com/rust-lang/rust/pull/102963 (`-Z instrument-xray`)
- https://github.com/rust-lang/rust/pull/91125 (LLVM plugin loading)
- https://github.com/rust-lang/rust/issues/92109 (`-Z instrument-mcount` breakage)
- https://github.com/rust-lang/rust/pull/151994 (v0 mangling as stable default)
- https://blog.rust-lang.org/2026/08/20/Rust-1.98.0/

**Compiler tooling / instrumentation prior art**
- https://github.com/cognitive-engineering-lab/rustc_plugin
- https://emavan.com/blog/2025/mir-instrumentation/
- https://github.com/jamesmth/llvm-plugin-rs

**eBPF**
- https://github.com/aya-rs/aya
- https://docs.rs/aya/latest/aya/programs/uprobe/struct.UProbe.html
- https://aya-rs.dev/book/programs/probes
- https://github.com/J00MZ/opentelemetry-rust-instrumentation (README, `Cargo.toml`, `CONTRIBUTING.md`, `docs/how-it-works.md`, and repository file listing read in full during verification)
- https://api.github.com/repos/open-telemetry/opentelemetry-rust-instrumentation (verification: confirmed HTTP 404 - no such repository exists in the `open-telemetry` org, despite J00MZ's `Cargo.toml`/`CONTRIBUTING.md` declaring it as the canonical location)
- https://api.github.com/repos/open-telemetry/opentelemetry-go-instrumentation (verification: confirmed HTTP 200 - the real, existing official Go eBPF project)

**Cargo build-caching behaviour (verification pass - includes a hands-on experiment, see [Appendix B](appendix-b-verification-log.md) item 2)**
- https://github.com/rust-lang/cargo/pull/9348 ("Don't re-use rustc cache when RUSTC_WRAPPER changes" - read in full; clarifies this PR fixed only an internal `rustc --version` probe cache, not the artifact rebuild fingerprint)
- https://github.com/rust-lang/rust-analyzer/issues/20275 (a related but distinct wrapper/cache-invalidation bug, ruled not directly applicable)
- https://doc.rust-lang.org/nightly/nightly-rustc/cargo/core/compiler/fingerprint/ (Cargo's own fingerprint module docs)
- Direct experiment: a native Rust `RUSTC_WRAPPER` binary built and run against a scratch crate on the local toolchain (`rustc 1.97.1`, `cargo 1.97.1`, Windows) - see Appendix B for the full protocol and results

**Adversarial review round (see [Appendix C](appendix-c-adversarial-review.md))**
- https://github.com/sourcefrog/cargo-mutants - the source of the byte-range splicing technique adopted in [ADR-002](17-decision-records.md); its own documentation states that mutations are applied textually so untouched code keeps its formatting, comments, and line numbers
- https://github.com/oxidecomputer/usdt (v0.6.0, ~3.8M downloads) and https://github.com/cuviper/probe-rs (v0.5.2, ~1.8M) - stable-Rust USDT probe emission; the evidence that "compile-time probe metadata embedded in a binary for eBPF" was never a novel mechanism ([Appendix C.4](appendix-c-adversarial-review.md))
- https://github.com/fast/fastrace - a third emitter candidate the original two-way analysis missed; its "10–100× faster" figure is the project's own tagline, not an independent benchmark ([Appendix C.7](appendix-c-adversarial-review.md))
- https://github.com/tokio-rs/tokio/pull/6793 and https://github.com/tokio-rs/tokio/pull/6891 - `tokio::task::Id` stabilisation, which refuted the original research's claim that Tokio exposes no stable task identity ([Appendix C.5](appendix-c-adversarial-review.md))
- https://github.com/dvc94ch/cargo-trace - checked and found dormant (40 stars, last pushed 2021-03-04); the review overstated it as a live competitor
- `dalibo/hud` - **cited by the adversarial review and confirmed not to exist** (HTTP 404). Recorded because a hallucinated citation is itself evidence about how much weight to give an unverified assertion
- Direct experiment: a three-crate `RUSTC_WRAPPER` setup (`otel_shim` / `victim` / `app`) built and run on the local toolchain to test both cross-crate injection mechanisms - see [Appendix E.1](appendix-e-experiment-matrix.md) E-4 and E-5

**Phase 0 completion audit (see [§16](16-instrumentation-semantics.md), [§17](17-decision-records.md), [Appendix E](appendix-e-experiment-matrix.md))**
- `opentelemetry::trace::FutureExt` - re-read as the normative reference for the async span lifecycle specified in [§16.7](16-instrumentation-semantics.md)
- Rust reference and release notes for **`unsafe extern "C"` blocks and `safe fn` items** (stable since 1.82; `unsafe extern` is the edition-2024 form) - the basis for the edition-sensitivity requirement in [§16.3](16-instrumentation-semantics.md) and for the [Appendix E](appendix-e-experiment-matrix.md) FE-3 experiment proposal
- Rust lint documentation for `unsafe_code` and error **`E0453`** (`allow` cannot override `forbid`) - the basis for [R26](13-technical-risks.md) and the `#![forbid(unsafe_code)]` exclusion in [§6.11](06-rust-specific-challenges.md)

**Maintainer correspondence (Q&A round - see [Appendix D](appendix-d-maintainer-qa.md))**

Direct answers from maintainers, obtained in the OpenTelemetry community Slack. These are primary sources of a different kind from the rest of this list: not published documents, and not independently re-checkable by a reader, so each claim sourced to them is attributed by name and SIG in the text.

- **Scott Gerring** (`#otel-rust`) - on async future handling in the native OTel API vs. `tracing`, and on `tracing-opentelemetry`'s context-synchronisation bridge. Reversed the §5.4 emitter decision (Appendix D.2)
- **Xabier Martinez** (`#otel-go`) - `otelc` CodSpeed compile-time benchmark figures, the scope of what `otelc` benchmarks (compile time, not runtime latency), and the automated latest-version compatibility workflow (Appendix D.3)
- **Nikola Grcevski** (`#otel-ebpf`) - confirmation that OBI has zero application-level uprobes for Rust and falls back to generic socket kprobes (Appendix D.4)
- **Giuseppe Ognibene** (`#otel-ebpf`) - the working Tokio async task reconstruction / context propagation prototype and its remaining edge cases (Appendix D.4)

Referenced upstream artifacts:
- `opentelemetry::trace::FutureExt` - `with_context` / `with_current_context` (the primary-source basis for Appendix D.2, independent of the maintainer's word)
- OBI issue **#1096** - "Rust Tokio context propagation"
- `otelc` `.github/workflows/test-latestlibrun.yaml`, issue **#406** (the workflow), issue **#565** (an auto-filed version-range tracking issue)

**Source-rewriting fidelity (verification pass - hands-on experiment)**
- Direct experiment: a `syn` 2 (full features) + `prettyplease` 0.2 round-trip tool, built locally and run against a representative Rust source sample exercising line comments, doc comments, attributes, `cfg`, derive, generics, `async fn`, `macro_rules!`, and irregular formatting - see [Appendix B](appendix-b-verification-log.md) item 7 for the full sample, output, and diff

---

← [Architecture Decision Records](17-decision-records.md) · [Contents](../../README.md) · [Appendix B - Verification Log](appendix-b-verification-log.md) →
