← [Appendix A — Sources](appendix-a-sources.md) · [Contents](README.md)

---

## Appendix B — Verification log

**This appendix replaces the original document's "Claims that were NOT verified" list.** That list named eight specific claims the initial research pass could not confirm. All eight were subsequently checked — most by re-reading primary sources more deeply, two (marked below) by building and running actual experiments rather than reading documentation. This log records what was checked, how, and what was found, including where the finding *changed* a conclusion elsewhere in the document (each such case links to the affected section).

Verification pass date: 2026-09-04, same day as the original research. Experiments were run against the locally installed toolchain: `rustc 1.97.1` / `cargo 1.97.1`, Windows.

---

### 1. `otelc`'s "no added runtime overhead" claim

**Original status:** Not opened; `docs/benchmarking.md` existed but was unread.

**What was done:** Fetched and read `docs/benchmarking.md` in full.

**Finding — [Fact]:** The file contains **no runtime overhead, binary-size, or timing numbers of any kind**. Its only quantitative content is a CI gate: `BENCH_MAX_OVERHEAD_PCT=150`, which fails a CI job when `otelc`'s *compile time* (not runtime) exceeds 150% of a plain `go build` baseline measured in the same run. It documents three compile-time benchmark scenarios (baseline, multi, largeidle) and how to run them locally — methodology, not results.

**Consequence:** The "no added runtime overhead" claim in `otelc`'s v1 announcement is unsubstantiated by the project's own published materials, even in a document specifically about benchmarking. This strengthens (rather than merely repeats) the document's existing instruction not to make unmeasured overhead claims about our own tool, and gives us a concrete, precedented number to reuse: a 150%-of-baseline compile-time budget is a reasonable CI gate to adopt (see [§14.3](14-evaluation-plan.md)).

Affected sections: [§2.9](02-otelc-go.md).

---

### 2. Does `RUSTC_WRAPPER` participate in Cargo's build fingerprint? — verified by direct experiment

**Original status:** Open question (O1 / Q1), flagged as the single highest-priority thing to test before any implementation work.

**This is the most consequential item in this log** — it upgrades the document's biggest Critical risk (R1) from "hypothetical, verify first" to "confirmed, here is the fix."

**What was done:** A real experiment, not a documentation lookup, since Cargo's fingerprinting behavior is not clearly specified in prose and the tool that would be affected is the one this whole project depends on.

1. Built a scratch binary crate (`cargo new hello`).
2. Built a native `RUSTC_WRAPPER` executable in Rust (a shell script does not work as `RUSTC_WRAPPER` on Windows — Cargo invokes it directly via `CreateProcess`, which requires a real PE executable, not a script) that logs every invocation it receives (including full argument list) to a file, then execs the real `rustc` with the same arguments and forwards the exit code.
3. `cargo clean`, then `cargo build` with **no** wrapper set. Recorded the resulting fingerprint hash (`target/debug/.fingerprint/hello-<hash>`).
4. Ran `cargo build` again, **now with `RUSTC_WRAPPER` pointed at the wrapper binary**, with the crate's source completely unchanged.
5. Repeated the same experiment with `RUSTC_WORKSPACE_WRAPPER` instead of `RUSTC_WRAPPER`.
6. As a control, repeated the toggle using `RUSTFLAGS` instead of a wrapper variable, to confirm the fingerprinting mechanism *can* detect environment changes when it is designed to.

**Findings — [Fact], directly observed:**

- Step 4 produced **`Finished` with no `Compiling` line** — Cargo did not recompile the crate. The wrapper log showed it was invoked only for two harmless internal probe calls (`rustc -vV`, a `--print=sysroot`/`--print=cfg`/etc. target-info query) — **never for the actual compilation of `hello`**. The fingerprint hash in step 4 was byte-identical to the one from step 3.
- Step 5 (`RUSTC_WORKSPACE_WRAPPER`) showed the **same behaviour**: no recompilation, wrapper not invoked for the actual compile, identical fingerprint hash.
- As a positive control: touching the source file and rebuilding with the wrapper set **did** trigger a real recompile, and the wrapper log showed the full real `rustc` invocation (`--crate-name hello --edition=2024 src\main.rs ...`) — confirming the wrapper mechanism itself works correctly; it is specifically *toggling the wrapper on/off with no other change* that Cargo fails to notice.
- Step 6 (`RUSTFLAGS`): setting `RUSTFLAGS="--cfg instrumented"` on a previously-built crate **did** force a full recompile, and changing the flag's value again (`--cfg instrumented2`) forced another recompile. Three consecutive `RUSTFLAGS` changes produced three consecutive `Compiling` lines.

**Cross-checked against a primary source:** [rust-lang/cargo#9348](https://github.com/rust-lang/cargo/pull/9348), "Don't re-use rustc cache when RUSTC_WRAPPER changes," read in full. Its description: *"We check the mtime of `rustc` to bust the cache if the compiler changed. However, before this PR, we didn't look at mtimes of `RUSTC_WRAPPER` / `RUSTC_WORKSPACE_WRAPPER`, so we could've re-used old cache with new wrapper."* Critically, this PR fixed a **different, narrower cache** than the one governing whether a crate gets recompiled: it fixed Cargo's internal cache of `rustc --version` output (used to speed up repeated no-op invocations), by making that specific cache check the wrapper binary's own mtime. It did **not** add the wrapper's identity, path, or presence/absence to the artifact rebuild fingerprint. This is fully consistent with the experiment: the wrapper binary's mtime did not change between steps 3 and 4, so even the fix from #9348 (already present in cargo 1.97.1) had nothing to invalidate — and the underlying artifact-fingerprint gap that PR left untouched is exactly what the experiment reproduced. (A related but distinct bug, [rust-analyzer#20275](https://github.com/rust-lang/rust-analyzer/issues/20275), was also checked and ruled not applicable — it concerns a wrapper that skips compilation and leaves stale `.d` files, not the fingerprint-inclusion question.)

**Conclusion — [Fact]:** Cargo's rebuild fingerprint does not include whether `RUSTC_WRAPPER`/`RUSTC_WORKSPACE_WRAPPER` is set, unset, or changed. In the specific and very likely scenario of "a crate was already built once, then the user turns instrumentation on," Cargo will silently serve the old, uninstrumented artifact and report success. This is the worst possible failure mode for an observability tool: it looks like it worked.

**Validated mitigation — [Fact]:** Changing `RUSTFLAGS` reliably busts the cache. The tool must derive a synthetic `RUSTFLAGS` value (e.g. a `--cfg` carrying a hash of the active rule set + tool version) and set it whenever the instrumentation configuration is active, so that turning instrumentation on/off, or changing which rules apply, always changes `RUSTFLAGS` and therefore always forces the correct recompile. This must ship in the tool's first working version, not be added as later hardening — see [§13, R1](13-technical-risks.md) and [§15.3](15-final-recommendation.md).

Affected sections: [§12.9 O1](12-mvp-definition.md), [§13, R1](13-technical-risks.md), [§15.5 Q1](15-final-recommendation.md), [§15.6](15-final-recommendation.md), [§1.5](README.md) confidence table.

---

### 3. `J00MZ/opentelemetry-rust-instrumentation`'s internal implementation

**Original status:** README only; source unread; described as "a personal repository, not an `open-telemetry` org project."

**What was done:** Read the repository's file listing via the GitHub API, then fetched and read in full: `README.md` (complete, not the truncated excerpt from the first pass), `Cargo.toml`, `CONTRIBUTING.md`, and `docs/how-it-works.md`. Cross-checked the `repository` field in `Cargo.toml` by querying `api.github.com/repos/open-telemetry/opentelemetry-rust-instrumentation` directly (HTTP 404) and `api.github.com/repos/open-telemetry/opentelemetry-go-instrumentation` (HTTP 200).

**Findings — [Fact], several of which correct the original research pass:**

- It is a **pure-Rust** project built on **Aya** (`aya = "0.13"`, features `["async_tokio"]`), not a Go-based tool — the original description was accurate on this point but under-specified.
- Its `Cargo.toml` declares `repository = "https://github.com/open-telemetry/opentelemetry-rust-instrumentation"`, and `CONTRIBUTING.md` instructs contributors to `git clone` that same URL. The project explicitly presents itself as — or aspires to become — the official OTel Rust eBPF instrumentation project, and its README lists [`open-telemetry/opentelemetry-go-instrumentation`](https://github.com/open-telemetry/opentelemetry-go-instrumentation) (the real, existing official Go eBPF auto-instrumentation project, confirmed HTTP 200) as its direct "inspiration."
- **`open-telemetry/opentelemetry-rust-instrumentation` does not exist** (confirmed HTTP 404). The aspiration is not yet realized; the project currently lives only at the personal `J00MZ` URL. This was not previously checked — the original pass only characterized it as "a personal repository," which is true but did not surface the explicit intent to become an OTel-org project.
- **Mechanism, from `docs/how-it-works.md`:**
  - Symbol table scan → `rustc-demangle` → per-library pattern matching against a fixed target list (e.g. `hyper::server::conn::Http::serve_connection`).
  - **Multi-return-point uprobes**: rather than a single `uretprobe`, it locates every `ret` instruction in a target function and places a uprobe at each — an explicit, practical acknowledgment that Rust functions commonly have multiple exit paths that a single return-probe would miss. This is new information not discussed in the original research pass's treatment of uprobes.
  - Struct layout knowledge via JSON offset maps keyed by library version, falling back to DWARF parsing, falling back to undocumented heuristics — direct confirmation that the "fragile symbol/offset discovery" problem discussed in [§7.4](07-ebpf-future.md) is a real, current engineering burden for this project, not a hypothetical one.
  - Async correlation strategy stated as "instrument at the executor level and track task contexts to maintain proper span hierarchies" — i.e. this project is already attempting the problem posed as Hypothesis H2 (§7.5), with no published accuracy metrics.
- **Coverage reality check**: the workspace in `Cargo.toml` lists BPF instrumentor members for `hyper`, `tonic`, `reqwest`, and `axum`, but only `pkg/instrumentors/bpf/hyper/` and `pkg/instrumentors/bpf/tonic/` exist as populated directories at time of investigation. `reqwest` and `axum` support is README-only. Context propagation is explicitly listed as future work, not implemented.
- Repository activity: created 2026-01-06, last pushed 2026-09-02 (active), 0 stars, not archived.

**Consequence:** This project is materially more relevant to §7's eBPF discussion than the original one-paragraph treatment suggested — both as evidence that OTel's own ecosystem is already reaching for this idea, and as the best available real-world data point on Hypothesis H2.

Affected sections: [§4.5](04-rust-instrumentation-landscape.md), [§7.4–7.5](07-ebpf-future.md), [§15.5 Q8](15-final-recommendation.md), [§15.6](15-final-recommendation.md).

---

### 4. Current resolution status of opentelemetry-rust#1571

**Original status:** Marked open, described as "open and unresolved as a GA blocker."

**What was done:** Queried the GitHub API for the issue's current state and read all 47 comments, focusing on the most recent ones. Followed the trail to the linked follow-up PR.

**Findings — [Fact]:**

- The issue is **closed** (`state_reason: completed`, closed 2026-03-18).
- Resolution, per maintainer scottgerring's 2025-04-01 "Path Forward" comment: **"Option 3 — Maintain Both APIs."** Both the OTel-native tracing API and the `tracing` crate remain first-class; the goal was fixing interoperability (tracked separately as the "context synchronisation issue" in [#1690](https://github.com/open-telemetry/opentelemetry-rust/issues/1690)), not deprecating either.
- Closing comment (2026-03-18): *"Interop works now; unified context would be nice but seems unrealistic. Let's put a line under it."*
- The follow-up action was [PR #3122](https://github.com/open-telemetry/opentelemetry-rust/pull/3122) (merged), which added `docs/traces.md` (marked Work-In-Progress, since traces remain Beta) and `docs/logs.md` (marked Stable) to the repository. **`docs/traces.md` was fetched and read**; it states: *"For new code, prefer the OpenTelemetry Tracing API directly,"* citing `tracing`'s lack of span kind, links, and remote-parent support.

**Consequence — a genuine complication, not a clean resolution in our favour:** The good news is that `tracing` is not going away — the risk framed in the original document (R15: "#1571 resolves against tracing") does not materialize as stated. The complication is new: the project's own **current, official guidance for new code recommends the opposite of what §5.4 recommends we generate**. This does not automatically overturn §5.4's recommendation — the reasoning there (async correctness via the `Instrumented` future, `STATIC_MAX_LEVEL`, ecosystem convergence with `tokio`/`hyper`/`axum`) is about risk for a tool generating code automatically at scale, which is a different question from "what should a human write by hand" — but it means we are now making a **considered, stated departure from upstream guidance**, not merely picking whichever API happens to be safe. This has been written into §5.2/§5.4 directly rather than left as a footnote.

Affected sections: [§5.2](05-otel-rust.md) (substantially rewritten), [§5.4](05-otel-rust.md) (recommendation reasoning and costs both updated), [§15.5 Q10](15-final-recommendation.md), [§1.5](README.md) confidence table (confidence revised down from "Medium-high" to "Medium").

---

### 5. Exact `code.*` semantic-convention attribute names and stability

**Original status:** Open question; told to check before Phase 1 implementation.

**What was done:** Fetched the OpenTelemetry semantic conventions registry page for the `code` attribute group directly.

**Finding — [Fact]:** The `code.*` group was promoted to **Stable** in semconv v1.33.0. Current stable names (renamed from earlier experimental ones): `code.function.name` (fully-qualified name without arguments; replaces the old split between `code.namespace` and a bare function name), `code.file.path` (replaces `code.filepath`), `code.line.number` (replaces `code.lineno`), `code.column.number`, and `code.stacktrace`. A migration guide exists for instrumentation authors moving off the old names.

**Consequence:** Phase 1 has a confirmed, stable attribute schema to target for the generic function-instrumentation spans it produces — no further research needed here before implementation.

Affected sections: [§5.3](05-otel-rust.md), [§12.9 O6](12-mvp-definition.md).

---

### 6. Does OBI attach function-level uprobes to Rust application code, or only syscall/TLS-boundary probes?

**Original status:** Open question; the original document only characterized OBI's *context propagation* mechanism for Rust (network-level only), without establishing what OBI does or does not instrument in Rust application code at all.

**What was done:** This required triangulating multiple sources, since no single official page states it plainly. Checked: the OBI overview page (confirmed it does not contain language-specific mechanism detail), Grafana's Beyla documentation and blog posts, Splunk's OBI blog post, and — most usefully — a DeepWiki-generated architecture page for `open-telemetry/opentelemetry-ebpf-instrumentation`, which is derived from and cites the actual source tree.

**Finding — [Fact], corroborated across sources]:** OBI has two distinct tracer implementations:

- A **Go Tracer**, which attaches **uprobes directly to Go-specific library internals** (`net/http`, gRPC) and uses version-keyed struct-offset resolution to handle Go's evolving internal layouts.
- A **Generic Tracer**, used for every other language including Rust, C, and C++, which works purely via **kprobes on kernel socket syscalls** (`security_socket_accept`, `sys_accept4`, and similar) plus **socket filters** for L7 protocol parsing. Uprobes in the Generic Tracer path are reserved for a small, specific set of shared libraries (OpenSSL for TLS, Nginx) — not application code.

In other words: **OBI attaches zero function-level uprobes into Rust application code.** Rust is treated identically to any other natively-compiled language with no bespoke tracer — the same class of treatment C and C++ receive. This is a materially sharper (and less flattering) claim than "Rust gets network-level propagation only," which could be misread as "Rust gets most of what Go gets, minus one feature." It does not.

**Supporting evidence:** Grafana's own Beyla documentation cautions: *"For non-Go services, especially asynchronous or reactive frameworks, you should validate Beyla trace support before deploying to production."*

**Consequence:** This sharpens the eBPF gap analysis in §7 considerably. There is no existing baseline of Rust application-level uprobes for a compiler-metadata-driven approach to be merely *incremental* over — the comparison point is not "OBI, but better," it is "nothing, versus a from-scratch third-party project" (J00MZ, item 3 above). This also reframes Hypothesis H1 (§7.4): the relevant comparison for "does compiler metadata improve probe selection" is against hand-built symbol matching (as J00MZ does it), not against an existing OBI baseline that does not exist for Rust.

Affected sections: [§4.5](04-rust-instrumentation-landscape.md), [§7.5](07-ebpf-future.md) (Established facts and H1 both revised).

---

### 7. Does `syn` round-trip real-world Rust source with sufficient fidelity to rewrite third-party crates? — verified by direct experiment

**Original status:** Open question (O2); the document speculated that `syn` "loses formatting and drops some tokens on round-trip" without measurement.

**What was done:** A second hands-on experiment, because this claim directly gates whether Architecture A's core mechanism (source rewriting) is viable, and "loses formatting" was previously an assumption, not a measurement.

1. Built a small Rust tool (`syn` 2, full features, + `prettyplease` 0.2) that parses a file with `syn::parse_file` and re-emits it with `prettyplease::unparse` — i.e. exactly the parse→transform→print pipeline Architecture A proposes.
2. Wrote a representative ~40-line sample file deliberately exercising every construct relevant to the tool's design: a leading line comment, an inner doc comment (`//!`), a struct with an outer doc comment (`///`) and a field-level line comment, `#[derive]` and `#[cfg]` attributes, `pub(crate)` visibility, a method with an inline implementation comment, an `async fn` with `#[tracing::instrument(skip(self))]` and a trailing inline comment, a `macro_rules!` definition, and a function with irregular/messy manual formatting.
3. Ran the tool on the sample, diffed input against output, then ran the tool **again on its own output** to check idempotence.

**Findings — [Fact], directly observed:**

- **All four plain `//` line comments were silently deleted** — the leading module comment, the field-level comment, the trailing inline comment, and the implementation comment. 4 for 4 lost, with no error or warning.
- **Both doc comments (`///`, `//!`) were fully preserved**, because they desugar to `#[doc = "..."]` attributes, which are real tokens the tokenizer retains — comments proper are not tokens at all in `proc-macro2`/`syn` and are simply never seen.
- **All blank lines between items were collapsed**, and the entire file was reformatted to `prettyplease`'s own canonical style — not just the constructs a real tool would touch. A `Self { name: name.into(), retries: 3 }` literal that fit on one line in the source was reformatted to three lines; deliberately messy manual spacing (`fn weird_formatting(  a:i32,b :i32   )->i32{`) was normalized to canonical style. This means an instrumented file's diff against its original will look like a full-file reformat, not a surgical one-line insertion, regardless of how small the actual instrumentation change is.
- **All structural content survived intact**: attributes, `cfg`, `derive`, `pub(crate)` visibility, generic bounds (`impl Into<String>`), the `async fn` signature and its `#[tracing::instrument(...)]` attribute, and the full `macro_rules!` body all round-tripped with no semantic loss.
- **The round-trip is idempotent**: running the tool on its own output a second time produced a byte-identical result. Whatever damage occurs, it occurs once and then stabilizes.

**Consequence:** This replaces a hedge ("syn loses formatting") with a specific, quantified cost: **complete loss of non-doc comments, plus a full-file reformat, on every touched file, always.** This does not block Architecture A — none of the losses are semantic, and the output still compiles — but it is a real, now-documented user-experience cost that must be disclosed prominently (a user whose comments silently vanish will distrust the tool regardless of whether the binary is correct), and it sharpens the remaining open question: whether `ra_ap_syntax`'s lossless CST is worth adopting specifically to avoid this, now that the `syn`+`prettyplease` baseline it would be compared against is known rather than assumed.

Affected sections: [§12.9 O2](12-mvp-definition.md), [§13, R11](13-technical-risks.md), [§15.5 Q3](15-final-recommendation.md), [§15.6](15-final-recommendation.md), [§1.5](README.md) confidence table.

---

### 8. Hypothesis H2 (logical async spans reconstructible from poll events + metadata)

**Original status:** Explicitly flagged as the project's central, load-bearing, unvalidated research hypothesis — not resolvable by documentation research alone, since no one has published results either way.

**What was done:** This cannot be "verified" by research; it requires a working prototype, which is out of scope for Phase 0. What verification *could* do, and did, was look for existing evidence of anyone attempting it.

**Finding:** See item 3 above. [J00MZ/opentelemetry-rust-instrumentation](https://github.com/J00MZ/opentelemetry-rust-instrumentation) states its async strategy as "instrument at the executor level and track task contexts to maintain proper span hierarchies" — a real, current attempt at exactly this problem, using runtime heuristics with no compiler assistance, with no published accuracy data.

**Status: still unresolved, correctly.** This neither confirms nor refutes H2. It does two useful things: it confirms the problem is being actively worked on elsewhere (raising confidence that it is a real, non-trivial, currently-open problem rather than something already solved that we would be redundantly re-attempting), and it gives Phase 3 a concrete artifact to study — reading J00MZ's actual implementation and any results it produces is now a well-defined, cheap first step before building an independent prototype.

Affected sections: [§7.4–7.5](07-ebpf-future.md), [§15.5 Q8](15-final-recommendation.md), [§15.6](15-final-recommendation.md).

---

### Summary table

| # | Claim | Original status | Resolution method | New status |
| --- | --- | --- | --- | --- |
| 1 | `otelc` runtime overhead numbers | Unread source | Read `docs/benchmarking.md` in full | **[Fact]** — confirmed empty of runtime data; only a compile-time CI gate exists |
| 2 | `RUSTC_WRAPPER` and Cargo's fingerprint | Open question | **Hands-on experiment** + primary-source PR read | **[Fact]** — confirmed does not participate; mitigation validated |
| 3 | J00MZ project internals | README only | Read source, `Cargo.toml`, docs; cross-checked repo existence via API | **[Fact]** — mechanism, aspirations, and coverage gaps all documented |
| 4 | opentelemetry-rust#1571 status | "Open, unresolved" | Read closed issue + 47 comments + follow-up PR + resulting doc | **[Fact]** — closed, resolved, but surfaced a new tension with current guidance |
| 5 | `code.*` semconv names | Open question | Read the registry page | **[Fact]** — stable names confirmed |
| 6 | OBI's Rust probe mechanism | Not established | Cross-referenced architecture docs | **[Fact]** — kprobes/socket filters only, zero application uprobes |
| 7 | `syn` round-trip fidelity | Assumed, unmeasured | **Hands-on experiment** | **[Fact]** — comment loss and full reformat quantified precisely |
| 8 | H2 (async span reconstruction) | Unvalidated hypothesis | Searched for prior attempts | **Still [Hypothesis]** — but now with a concrete real-world data point |

Two of these eight (items 2 and 7) were resolved by building and running actual code rather than reading about the problem, because the underlying documentation was either silent or actively misleading on both points. Both experiments are small, reproducible, and worth re-running against a different Cargo/rustc version before Phase 1 begins, since Cargo's fingerprinting behaviour in particular is exactly the kind of internal detail that could change between releases without being called out as a breaking change.

---

← [Appendix A — Sources](appendix-a-sources.md) · [Contents](README.md)
