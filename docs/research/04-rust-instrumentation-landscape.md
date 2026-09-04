← [The Rust Compilation Pipeline](03-rust-compiler-pipeline.md) · [Contents](../../README.md) · [OpenTelemetry Rust](05-otel-rust.md) →

---

## 4. Existing Rust Instrumentation Landscape


### 4.1 `tracing` (tokio-rs)

- Repo: [tokio-rs/tracing](https://github.com/tokio-rs/tracing) · crates.io: `tracing` **0.1.44**, ~818M downloads, last published 2025-12-18.
- **Layer:** application/library source, via macros.
- **Automatic?** No.
- **Compile-time?** The macros are compile-time; the *decision* to instrument is manual.
- **OTel?** Not directly; via a bridge.
- **eBPF?** No.

**[Fact]** `tracing` is the de facto structured-diagnostics framework for Rust. Its span model is a scoped, nestable, attributed context with an explicit enter/exit lifecycle, distinct from an event. Subscribers/layers consume spans; `tracing-subscriber` provides the layering machinery.

**[Fact]** `tracing` supports compile-time level filtering via Cargo features (`release_max_level_off`, `…_error`, `…_warn`, `…_info`, `…_debug`, `…_trace`), which set the `STATIC_MAX_LEVEL` constant that the macros check. Instrumentation at statically disabled levels is not present in the resulting binary at all ([`tracing::level_filters`](https://docs.rs/tracing/latest/tracing/level_filters/index.html)).

**[Inference — directly relevant, corrected after the adversarial review round]** This gives an auto-instrumentation tool an idiomatic "off switch" for zero runtime cost — but **not a per-tool one**, and this was overstated in the original research. `STATIC_MAX_LEVEL` is a single global constant, additive across the whole compiled dependency graph: setting `release_max_level_info` in the final binary silences *every* `DEBUG`-and-below span in the program, including the user's own hand-written ones and any third-party crate's, not just the ones our tool generated. See [Appendix C.1](appendix-c-adversarial-review.md). ~~It is still a strong argument for generating `tracing` calls rather than raw OTel API calls.~~ **[Superseded — [Appendix D.2](appendix-d-maintainer-qa.md).]** It was the strongest surviving argument for `tracing` after the async argument fell, and it did not carry the decision: a tool-specific kill switch needs a dedicated `--cfg` gate on generated code either way, and a `--cfg`-gated splice is deleted at compile time regardless of which API it calls. What is genuinely lost by generating the native OTel API is the *middle* configuration — instrumentation present but runtime-disabled, which now costs a non-recording span rather than nothing ([R23](13-technical-risks.md)).

### 4.2 `#[tracing::instrument]` (`tracing-attributes`)

- Docs: [`tracing_attributes::instrument`](https://docs.rs/tracing-attributes/latest/tracing_attributes/attr.instrument.html).
- **Layer:** proc macro, AST.
- **Automatic?** No — must be written on each function.
- **Compile-time?** Yes.
- **OTel?** Indirectly.
- **eBPF?** No.

**[Fact]** `#[instrument]` creates and enters a span on function call, named after the function, at `INFO` by default, recording all arguments as fields (via `Value` where implemented, otherwise `Debug`). `skip`/`skip_all` exclude arguments. `ret` emits an event with the return value; `err` records `Err` variants. **[Fact]** For `async fn`, the macro wraps the body in an `Instrumented` future so the span is entered and exited around each `poll`, preserving span context across `.await` points. **[Fact]** `const fn` cannot be instrumented and causes a compile error.

~~**[Inference]** `#[instrument]` is the correct *code generation target* for a source-level tool.~~ **[Superseded — [Appendix D.2](appendix-d-maintainer-qa.md) / [ADR-001](17-decision-records.md).]** Two of the three supporting reasons did not survive. *"It already solved the async problem correctly"* is true but not exclusive — `opentelemetry::trace::FutureExt::with_context` implements the same attach-per-`poll()` / detach-on-yield lifecycle ([§5.3](05-otel-rust.md)), which was the load-bearing claim behind the original emitter choice. *"Our output is idiomatic, reviewable, and debuggable"* is neutral rather than an advantage: a generated `tracer.start(...)` or `.with_context(cx)` is equally readable, and unlike an attribute macro it needs no `cargo expand` to see what it actually does. **The tool generates native OpenTelemetry API calls.** `#[instrument]` remains what this section describes — the mature, correct, manual option — and stays available behind the [§11.4](11-recommended-architecture.md) emitter seam for users who want generated spans inside their existing `tracing` tree.

### 4.3 OpenTelemetry Rust

- Repo: [open-telemetry/opentelemetry-rust](https://github.com/open-telemetry/opentelemetry-rust).
- crates.io: `opentelemetry` **0.32.0** (~255M downloads), `opentelemetry_sdk` **0.32.1**, `opentelemetry-otlp` **0.32.0**, all last published May 2026.
- **Layer:** library API + SDK.
- **Automatic?** No.
- **OTel?** It *is* OTel.
- **eBPF?** No.

Detailed treatment in §5.

### 4.4 `tracing-opentelemetry`

- Repo: [tokio-rs/tracing-opentelemetry](https://github.com/tokio-rs/tracing-opentelemetry) · crates.io **0.33.0**, ~196M downloads, last published 2026-05-18.
- **Layer:** `tracing` subscriber layer.
- **Automatic?** No (but transparent once installed).
- **Compile-time?** No — runtime bridging.
- **OTel?** Yes.
- **eBPF?** No.

**[Fact]** Provides `OpenTelemetryLayer`, which attaches OTel context to `tracing` spans and emits them to OTel-compatible backends, plus `OpenTelemetrySpanExt` for injecting/extracting remote parent context and setting OTel-specific span attributes, status, and events. **[Fact]** Since 0.26 its version number runs one ahead of the `opentelemetry` crates it targets (e.g. `tracing-opentelemetry` 0.26 ↔ `opentelemetry` 0.25), which is a recurring source of user confusion and a real dependency-management hazard for any tool that injects both.

### 4.5 eBPF / runtime approaches touching Rust

#### OpenTelemetry eBPF Instrumentation (OBI), formerly Grafana Beyla

- Docs: [opentelemetry.io/docs/zero-code/obi/](https://opentelemetry.io/docs/zero-code/obi/) · [distributed traces](https://opentelemetry.io/docs/zero-code/obi/distributed-traces/) · donation: [open-telemetry/community#2406](https://github.com/open-telemetry/community/issues/2406).
- **Layer:** kernel (eBPF), on the running binary.
- **Automatic?** Yes.
- **Compile-time?** No.
- **OTel?** Yes, native.
- **eBPF?** Yes.

**[Fact]** OBI lists Rust among supported languages (Java, .NET, Go, Python, Ruby, Node.js, C, C++, Rust). **[Fact]** Its distributed-trace context propagation is split:

- **Library/memory-level injection** (`bpf_probe_write_user` into process memory), covering HTTP, HTTPS, HTTP/2 and gRPC — **Go only**.
- **Network-level injection** (Linux Traffic Control, injecting into HTTP headers and TCP/IP packets) — all languages, **disabled by default**, enabled via `BEYLA_BPF_CONTEXT_PROPAGATION=all`.

**[Fact]** Documented limitations of the network-level path: TLS/HTTPS traffic can only propagate through TCP/IP packet injection, which only works when *both* ends are instrumented by the same tool; gRPC and HTTP/2 are not supported at network level; L7 proxies and load balancers break TCP/IP-level propagation by discarding the original packets. **[Fact]** Requirements: kernel 5.10+ (with fixes) or 5.14+, and `CAP_NET_ADMIN`, `CAP_BPF`, `SYS_PTRACE` among others; the Go memory-injection path additionally requires that the kernel is not in integrity lockdown mode.

**[Inference — this is the crux of the eBPF gap]** For Rust, OBI produces network-boundary spans (RED metrics and HTTP/gRPC request spans derived from syscall and TLS-library probes), not application-function spans, and its context propagation is the weaker of its two mechanisms. The reason Go gets the better treatment is precisely that Go's runtime layout and goroutine structure are known and stable enough to write memory-level probes against. Rust has no such runtime — which is exactly why compile-time-produced metadata is an interesting lever (§7).

**[Fact — confirmed in the verification pass, see [Appendix B](appendix-b-verification-log.md) item 6]** This is sharper than "weaker propagation": OBI's architecture has two distinct tracer implementations. A **Go Tracer** attaches uprobes directly to Go-specific library internals (`net/http`, gRPC) using version-keyed struct-offset resolution. Everything else — including Rust — is handled by a **Generic Tracer** that works purely via kprobes on kernel socket syscalls (`security_socket_accept`, `sys_accept4`, etc.) plus socket filters for L7 protocol parsing, with uprobes used only for a small set of shared libraries (OpenSSL, Nginx). **Rust gets zero function-level, zero application-code uprobes from OBI today** — it is treated identically to any other natively-compiled language with no bespoke tracer, not as "Go with weaker propagation." Grafana's own Beyla documentation cautions that "for non-Go services, especially asynchronous or reactive frameworks, you should validate Beyla trace support before deploying to production."

#### `J00MZ/opentelemetry-rust-instrumentation`

- Repo: [J00MZ/opentelemetry-rust-instrumentation](https://github.com/J00MZ/opentelemetry-rust-instrumentation).
- **Layer:** eBPF uprobes on the target binary.
- **Automatic?** Yes (intended).
- **Compile-time?** No.
- **OTel?** Yes (intended).
- **eBPF?** Yes.

**[Fact]** Stated goal: "provide the same level of automatic instrumentation for Rust as exists for languages such as Java, Python, and Go," via eBPF uprobes. Pipeline: analyse the target binary for instrumentation points → resolve and demangle Rust symbols → attach uprobes at function boundaries → collect timing and context via perf events → convert to OTel spans. Claimed coverage: hyper, axum, tonic (client and server), reqwest. Claimed to work on Rust 1.70+ binaries including stripped release builds. Requirements: Linux 5.8+, `CAP_SYS_PTRACE`.

**[Fact — corrected in the verification pass; the original research pass under-described this project]** It is a pure-Rust implementation built on **Aya**, not a Go-based tool. Its own `Cargo.toml` declares `repository = "https://github.com/open-telemetry/opentelemetry-rust-instrumentation"` and its `CONTRIBUTING.md` instructs contributors to clone that same URL — i.e. the project explicitly presents itself as (or aspires to become) the official OTel Rust eBPF instrumentation project, mirroring the real, existing [`open-telemetry/opentelemetry-go-instrumentation`](https://github.com/open-telemetry/opentelemetry-go-instrumentation) (confirmed to exist, HTTP 200), which its README lists as direct "inspiration." **However, `open-telemetry/opentelemetry-rust-instrumentation` returns HTTP 404** — no such repository exists in the `open-telemetry` GitHub org. The project currently lives only at the personal `J00MZ` URL; whatever its aspirations, it has not (yet) been adopted upstream.

**[Fact — from reading the actual source/docs during verification]** Its documented mechanism, beyond the high-level pipeline above:

- **Symbol resolution:** scans the binary's symbol table, demangles with `rustc-demangle`, and pattern-matches decoded names against a fixed per-library target list (e.g. `hyper::server::conn::Http::serve_connection`).
- **Multi-return-point uprobes:** rather than a single `uretprobe`, it locates *every* `ret` instruction within a target function and places a uprobe at each — a direct, practical acknowledgment that Rust functions commonly have multiple exit paths (early returns, `?`, panics) that a single return-probe would miss.
- **Struct layout knowledge:** tracked via JSON offset maps keyed by library version, falling back to DWARF parsing or, failing that, undocumented "heuristics" — the exact "fragile symbol/offset discovery" problem discussed in §7.4, now confirmed as a real, current engineering burden rather than a hypothetical one.
- **Async correlation strategy:** stated directly as "instrument at the executor level and track task contexts to maintain proper span hierarchies" — i.e., this project is *already attempting* the problem posed as Hypothesis H2 in §7.5 (reconstructing logical async spans from poll-level events), using runtime heuristics with no compiler assistance and no stated accuracy metrics.
- **Coverage reality check:** the `Cargo.toml` workspace lists BPF instrumentor crates for `hyper`, `tonic`, `reqwest`, and `axum`, but at time of investigation only `pkg/instrumentors/bpf/hyper/` and `pkg/instrumentors/bpf/tonic/` exist as populated directories — `reqwest` and `axum` support is README-only. Context propagation across HTTP/gRPC headers is explicitly listed as future work, not implemented.

**[Fact]** Repository state at time of investigation: created 2026-01-06, last pushed 2026-09-02 (i.e., still active), 0 stars, not archived.

**[Inference — highly relevant, now much better supported]** This project is the closest existing thing to the eBPF half of our hypothesis, and it is attempting exactly the fragile parts: recovering semantic instrumentation points from a stripped binary by symbol analysis, *and* reconstructing async span structure without compiler help. Its immaturity (population of only 2 of 4 claimed library integrations, no accuracy metrics, propagation still unimplemented) is evidence the approach is hard; its existence, and its explicit modeling on the real official Go eBPF instrumentation project, is evidence the idea is a natural next step that OTel's own ecosystem is already reaching for. Its "instrument at the executor level" strategy for async is the single most directly relevant piece of prior art for H2 (§7.5) found in this investigation, and remains valuable architectural history for upstream collaboration on OBI #1096 even as our own eBPF branch is permanently closed ([ADR-005](17-decision-records.md)).

### 4.6 Adjacent crates checked and their verdicts

| Crate | Version / activity | What it is | Relevant? |
| --- | --- | --- | --- |
| `tracing-orchestra` / `-macros` | 0.2.x, last published 2023-09-12, ~4.7k downloads | Batch-applies `#[tracing::instrument]` and sets defaults for it | **Yes — closest prior art at source level.** Still opt-in per module/impl, still first-party-only, effectively unmaintained since 2023 |
| `instrument-level` | 1.0.21, updated 2026-07-22, ~4.4k downloads | Per-level convenience wrappers around `#[instrument]` | Marginal — ergonomics only |
| `otel-instrument` | 0.1.7, created and last updated Sept 2025, ~3.2k downloads | An `#[instrument]`-style macro targeting the OTel API directly rather than `tracing` | **Yes — a data point that people want OTel-native macros**; tiny and apparently dormant |
| `autometrics` | 3.0.0, updated 2025-09-18, ~4.2M downloads | Attribute macro adding Prometheus/OTel *metrics* (rate, error, duration) to functions | Adjacent: proves the "attribute macro → OTel telemetry" pattern is accepted at scale, but metrics-only and manual |
| `llvm-plugin-rs` | — | Write LLVM new-pass-manager passes in Rust | Relevant to Architecture D-variants only |
| `aya` | — | Pure-Rust eBPF library | Relevant to §7 |
| `rustc_plugin` / `rustc_utils` | 0.15.x-nightly-2026-05-01 | Framework for building rustc-API tools with Cargo integration | **Yes — this is what Architecture B would be built on** |

**Search scope and its limits.** **[Fact]** The negative result in §1.3 is based on: crates.io API lookups for the crates above; web searches for whole-crate Rust source rewriting, Rust MIR instrumentation, Rust compile-time OTel instrumentation, and Rust auto-instrumentation; the OTel zero-code language list; the OTel Rust SIG surface; and the OTel community issue tracker. **[Inference]** This is reasonable coverage of the public, discoverable ecosystem but does not rule out an unpublished internal tool at a vendor, a recently started repository with no search presence, or a crate whose description does not use the words we searched for. Treat "nothing exists" as *"nothing prominent exists,"* which is sufficient for our purposes.
---

---

← [The Rust Compilation Pipeline](03-rust-compiler-pipeline.md) · [Contents](../../README.md) · [OpenTelemetry Rust](05-otel-rust.md) →
