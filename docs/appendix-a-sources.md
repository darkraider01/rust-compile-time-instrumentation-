← [Final Recommendation](15-final-recommendation.md) · [Contents](../README.md) · [Appendix B — Verification Log](appendix-b-verification-log.md) →

---

## Appendix A — Primary sources consulted

Sources from the original research pass (2026-09-04) and the subsequent verification pass (2026-09-04, same day — see [Appendix B](appendix-b-verification-log.md) for what each new source resolved).

**OpenTelemetry Go compile-time instrumentation**
- https://github.com/open-telemetry/opentelemetry-go-compile-instrumentation
- https://github.com/open-telemetry/opentelemetry-go-compile-instrumentation/blob/main/docs/rules.md
- https://github.com/open-telemetry/opentelemetry-go-compile-instrumentation/blob/main/docs/implementation.md
- https://github.com/open-telemetry/opentelemetry-go-compile-instrumentation/blob/main/docs/getting-started.md
- https://github.com/open-telemetry/opentelemetry-go-compile-instrumentation/blob/main/docs/adr/0005-import-driven-instrumentation-selection.md
- https://github.com/open-telemetry/opentelemetry-go-compile-instrumentation/blob/main/docs/benchmarking.md (read in full during verification — confirmed to contain no runtime/binary-size numbers, only a CI compile-time-overhead gate)
- https://opentelemetry.io/docs/zero-code/go/compile-time/
- https://opentelemetry.io/blog/2026/go-compile-time-instrumentation-v1/
- Release metadata via the GitHub releases API (v1.0.0 2026-07-14, v1.1.0 2026-08-24)
- https://github.com/open-telemetry/opentelemetry-go-instrumentation (the official Go eBPF auto-instrumentation project — confirmed to exist; distinct from `otelc`, and the explicit "inspiration" cited by J00MZ/opentelemetry-rust-instrumentation)

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
- https://deepwiki.com/open-telemetry/opentelemetry-ebpf-instrumentation/3-architecture-overview (verification: Go Tracer vs. Generic Tracer split — confirms Rust receives kprobe/socket-filter treatment only, no application-level uprobes)

**OpenTelemetry Rust / tracing**
- https://github.com/open-telemetry/opentelemetry-rust
- https://github.com/open-telemetry/opentelemetry-rust/issues/1571 (re-read in full during verification via the GitHub API: confirmed **closed** 2026-03-18, resolved as "maintain both APIs")
- https://github.com/open-telemetry/opentelemetry-rust/issues/1690 (the "context synchronisation issue" tracked separately from #1571)
- https://github.com/open-telemetry/opentelemetry-rust/pull/3122 (verification: merged PR adding `docs/traces.md`/`docs/logs.md`)
- https://raw.githubusercontent.com/open-telemetry/opentelemetry-rust/main/docs/traces.md (verification: current official guidance — "For new code, prefer the OpenTelemetry Tracing API directly")
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
- https://api.github.com/repos/open-telemetry/opentelemetry-rust-instrumentation (verification: confirmed HTTP 404 — no such repository exists in the `open-telemetry` org, despite J00MZ's `Cargo.toml`/`CONTRIBUTING.md` declaring it as the canonical location)
- https://api.github.com/repos/open-telemetry/opentelemetry-go-instrumentation (verification: confirmed HTTP 200 — the real, existing official Go eBPF project)

**Cargo build-caching behaviour (verification pass — includes a hands-on experiment, see [Appendix B](appendix-b-verification-log.md) item 2)**
- https://github.com/rust-lang/cargo/pull/9348 ("Don't re-use rustc cache when RUSTC_WRAPPER changes" — read in full; clarifies this PR fixed only an internal `rustc --version` probe cache, not the artifact rebuild fingerprint)
- https://github.com/rust-lang/rust-analyzer/issues/20275 (a related but distinct wrapper/cache-invalidation bug, ruled not directly applicable)
- https://doc.rust-lang.org/nightly/nightly-rustc/cargo/core/compiler/fingerprint/ (Cargo's own fingerprint module docs)
- Direct experiment: a native Rust `RUSTC_WRAPPER` binary built and run against a scratch crate on the local toolchain (`rustc 1.97.1`, `cargo 1.97.1`, Windows) — see Appendix B for the full protocol and results

**Source-rewriting fidelity (verification pass — hands-on experiment)**
- Direct experiment: a `syn` 2 (full features) + `prettyplease` 0.2 round-trip tool, built locally and run against a representative Rust source sample exercising line comments, doc comments, attributes, `cfg`, derive, generics, `async fn`, `macro_rules!`, and irregular formatting — see [Appendix B](appendix-b-verification-log.md) item 7 for the full sample, output, and diff

---

← [Final Recommendation](15-final-recommendation.md) · [Contents](../README.md) · [Appendix B — Verification Log](appendix-b-verification-log.md) →
