← [Appendix D - Maintainer Q&A](appendix-d-maintainer-qa.md) · [Contents](../../README.md)

---

## Appendix E - Experiment matrix

Phase 0 settled several questions by **building and running code** rather than by reading documentation. Those experiments are recorded in narrative form in [Appendix B](appendix-b-verification-log.md) and [Appendix C](appendix-c-adversarial-review.md); this appendix consolidates them into one comparable table so the evidence base can be audited at a glance, and separates it cleanly from **source verification** (reading primary sources), **maintainer testimony** ([Appendix D](appendix-d-maintainer-qa.md)), and **future experiments** that have not been run.

**Nothing in §E.1 was invented.** Every row corresponds to code that was written and executed. **Nothing in §E.3 has been run** - those rows are proposals, and are labelled as such so they cannot be mistaken for results.

**Shared environment for every experiment in §E.1:**

| | |
| --- | --- |
| Toolchain | `rustc 1.97.1` / `cargo 1.97.1` |
| Platform | **Windows/MSVC** for E-1 through E-6; **Windows/MSVC and WSL2 Linux/ELF** for E-7 through E-10 (added below) |
| Date | 2026-09-04 |
| Documented baseline | Rust 1.98.0 stable is the project's stated toolchain baseline; experiments ran one minor version behind it |

**[Updated - [ADR-003](17-decision-records.md), [R24](13-technical-risks.md)]** The Linux leg of the cross-platform question is now closed (E-9). macOS remains untested - no local Mach-O environment - and is the one platform gap left before the "platforms eBPF cannot reach" positioning is fully earned.

---

### E.1 Experiments performed

#### E-1 · Does `RUSTC_WRAPPER` participate in Cargo's rebuild fingerprint?

| | |
| --- | --- |
| **Question** | If a crate is already built and the user then enables instrumentation, does Cargo rebuild it? |
| **Environment** | As above. Scratch binary crate; a **native** `RUSTC_WRAPPER` executable in Rust (a shell script does not work on Windows - Cargo invokes the wrapper via `CreateProcess`, which needs a real PE executable) that logs every invocation and execs the real `rustc` |
| **Method** | `cargo clean`; build with no wrapper; record the fingerprint hash. Rebuild with `RUSTC_WRAPPER` set and **no source change**. Repeat with `RUSTC_WORKSPACE_WRAPPER`. Positive control: touch the source and rebuild |
| **Result** | **[Fact]** `Finished` with **no** `Compiling` line. Wrapper invoked only for two internal probe calls (`rustc -vV`, a target-info query) - **never for the actual compile**. Fingerprint hash byte-identical. `RUSTC_WORKSPACE_WRAPPER` identical. Positive control passed: touching the source did trigger a real recompile with the full wrapped invocation |
| **Interpretation** | The wrapper's presence, absence, and identity are outside Cargo's artifact fingerprint. Cross-checked against [cargo#9348](https://github.com/rust-lang/cargo/pull/9348), read in full: that PR fixed Cargo's internal `rustc --version` cache, **not** the artifact rebuild fingerprint - consistent with the observation |
| **Architectural consequence** | [R1](13-technical-risks.md) upgraded from hypothetical to confirmed Critical. A cache-isolation mechanism must ship in the **first commit** ([§15.3](15-final-recommendation.md) step 1), because without it the tool silently produces uninstrumented binaries and reports success |
| **Remaining uncertainty** | Cargo-version-specific and undocumented; exactly the kind of internal behaviour that changes without a breaking-change note. Re-run on each toolchain bump. Windows-only observation |

#### E-2 · Does changing `RUSTFLAGS` reliably bust the cache?

| | |
| --- | --- |
| **Question** | Control for E-1: can *any* environment change force the rebuild, i.e. is the fingerprint mechanism working as designed and merely ignoring wrappers? |
| **Environment** | As E-1 |
| **Method** | On an already-built crate, set `RUSTFLAGS="--cfg instrumented"`, rebuild; change to `--cfg instrumented2`, rebuild; change again |
| **Result** | **[Fact]** Three consecutive `RUSTFLAGS` changes produced three consecutive `Compiling` lines |
| **Interpretation** | The fingerprint mechanism works; wrappers are simply not part of it. `RUSTFLAGS`-hashing is a *viable* mitigation |
| **Architectural consequence** | Originally adopted as the [R1](13-technical-risks.md) mitigation - **then rejected** on review, without any new experiment: `RUSTFLAGS` is global (evicting the entire workspace and dependency graph, build scripts included) and, as an environment variable, **clobbers** `[build] rustflags` in `.cargo/config.toml` rather than merging, silently discarding the user's sanitizer/target/link flags ([Appendix C.3](appendix-c-adversarial-review.md)). Superseded by E-6 and [ADR-004](17-decision-records.md) |
| **Remaining uncertainty** | None material. Worth recording that *"it works"* and *"it is the right mechanism"* are different findings, and this experiment only established the first |

#### E-3 · Does `syn` + `prettyplease` round-trip real Rust source faithfully?

| | |
| --- | --- |
| **Question** | Does the originally proposed parse → transform → print pipeline preserve source well enough to rewrite third-party crates? |
| **Environment** | As above; `syn` 2 (full features) + `prettyplease` 0.2 |
| **Method** | Build a round-trip tool (`parse_file` → `unparse`). Run it on a deliberately representative ~40-line sample exercising a leading line comment, `//!` and `///` doc comments, a field-level comment, `#[derive]`, `#[cfg]`, `pub(crate)`, an inline implementation comment, an `async fn` with `#[tracing::instrument(skip(self))]` and a trailing comment, a `macro_rules!` definition, and deliberately messy manual formatting. Diff input against output; then re-run on the output to test idempotence |
| **Result** | **[Fact]** **All 4 plain `//` comments silently deleted**, with no error or warning. Both doc comments fully preserved (they desugar to `#[doc]` attributes, which are real tokens; comments proper are never tokens in `proc-macro2`/`syn`). All blank lines collapsed; the **entire file** reformatted to canonical style, not just the touched item. **All structural content survived** - attributes, `cfg`, `derive`, visibility, generic bounds, the `async fn` signature, the full `macro_rules!` body. Round-trip idempotent |
| **Interpretation** | The damage is real, total for non-doc comments, whole-file in scope, and **non-semantic** - the output still compiles and behaves identically. So this does not block source rewriting; it makes the *output unusable for debugging*, which is nearly as bad ([R12](13-technical-risks.md)) |
| **Architectural consequence** | Replaced a hedge with a measurement, and then a mechanism: [ADR-002](17-decision-records.md) adopts `cargo-mutants`' technique - `syn` for **analysis only**, edit the original UTF-8 buffer at `span().byte_range()`. Removes `ra_ap_syntax` from consideration entirely and substantially defuses [R11](13-technical-risks.md) |
| **Remaining uncertainty** | The byte-splicing replacement is **not itself experimentally validated here** - it is adopted on `cargo-mutants`' documented, working precedent. `span().byte_range()`'s reliability across all constructs is a Phase 1 assumption |

#### E-4 · Can a stable-Rust wrapper instrument an undeclared dependency by injecting a crate dependency? *(Mechanism 1)*

| | |
| --- | --- |
| **Question** | The adversarial review's headline claim: that Cargo will not pass `--extern` for a crate the dependency does not declare, making dependency instrumentation *"impossible on stable Rust"* |
| **Environment** | As above. Three crates: `otel_shim` (runtime, built standalone), `victim` (does **not** declare `otel_shim` in its `Cargo.toml`), `app` (depends only on `victim`). Native `RUSTC_WRAPPER` intercepting `victim`'s compilation |
| **Method** | Wrapper rewrites `victim`'s source to add an instrumented function and **appends** `-L dependency=<shim_dir>` and `--extern otel_shim=<path>` to the `rustc` argv |
| **Result** | **[Fact]** `victim` compiled; `app` ran; the injected probe fired (`[shim] enter victim::probe`). **No `E0433`.** A constraint the review did not anticipate surfaced instead: the first attempt failed **downstream** with `E0463: can't find crate for otel_shim which victim depends on` - the injected dependency propagates into crate metadata, so every downstream consumer also needs `-L` |
| **Interpretation** | The review's premise was correct (Cargo does not pass the flag) and its conclusion did not follow, because a wrapper **rewrites the argv** rather than passively forwarding it. Mechanism works, but drags a metadata-propagation obligation across the whole downstream graph |
| **Architectural consequence** | Refuted the "fatal structural flaw" claim. **Not adopted** - E-5 achieves the same result without the propagation obligation or the duplicate-crate/cycle hazard class ([ADR-003](17-decision-records.md)) |
| **Remaining uncertainty** | Single-file crates only; no diamond dependencies, no feature unification, no version conflicts between an injected shim and a real one |

#### E-5 · Can a stable-Rust wrapper instrument an undeclared dependency via an `extern "C"` trampoline? *(Mechanism 2 - adopted)*

| | |
| --- | --- |
| **Question** | Can instrumented code inside an unmodified third-party crate reach a runtime **without any Cargo graph involvement at all**? |
| **Environment** | As E-4 |
| **Method** | Wrapper splices **only** a symbol declaration - `unsafe extern "C" { fn __otel_span_enter(name: *const u8, len: usize); }` - into `victim`'s source, and appends **no flags whatsoever**. The symbol is expected to resolve at `app`'s final link, from a runtime the *application* declares |
| **Result** | **[Fact]** `victim` compiled, `app` linked and ran, the probe fired (`[runtime] span enter: victim::probe`). No `--extern`, no `-L`, no manifest edit, no `-Zunstable-options` |
| **Interpretation** | Dependency coverage - [§9.5](09-gap-analysis.md)'s *"single test that separates a real tool from a wrapper"* - works on stable Rust with no Cargo DAG involvement. Structurally cannot produce the duplicate-crate or dependency-cycle errors that constrain E-4, and mirrors `otelc`'s own trampoline indirection ([§2.5](02-otelc-go.md)) |
| **Architectural consequence** | **Adopted as the dependency-coverage mechanism** ([ADR-003](17-decision-records.md), [§12.1a](12-mvp-definition.md)). Pulled a one-dependency slice forward into the Phase 1 MVP ([§12.1](12-mvp-definition.md)), since the mechanism was no longer speculative |
| **Remaining uncertainty** | **Substantial, and load-bearing.** (a) The instrumented function was **synchronous** - Tier-2 async is unproven (FE-2). (b) Single-file crate; no `include!`, `#[path]`, or `build.rs`-generated modules (FE-8). (c) No LTO, `codegen-units=1`, or `panic=abort` (FE-1). (d) Windows/MSVC only (FE-7). (e) The target crate did not carry `#![forbid(unsafe_code)]`, which would have made the splice a hard compile error (FE-3) |

#### E-6 · Does an isolated `--target-dir` give cache isolation without touching user flags?

| | |
| --- | --- |
| **Question** | Can [R1](13-technical-risks.md) be solved without the collateral damage E-2's mitigation causes? |
| **Environment** | As above |
| **Method** | Run `cargo build --target-dir target/instrumented` with `RUSTC_WRAPPER` set; observe whether the wrapper is invoked for every crate, and whether the default `target/` cache survives |
| **Result** | **[Fact]** The wrapper was invoked for **every** crate - a fresh directory contains no uninstrumented artifacts to reuse - and the default `target/` cache was left intact |
| **Interpretation** | Solves cache isolation and the stale-artifact problem together, without global flags and without clobbering `.cargo/config.toml` |
| **Architectural consequence** | **Adopted** ([ADR-004](17-decision-records.md)). Replaces the `RUSTFLAGS`-hash mitigation in [R1](13-technical-risks.md), [§12.9](12-mvp-definition.md) O1, and [§15.5](15-final-recommendation.md) Q1 |
| **Remaining uncertainty** | Does **not** address invalidation *within* the instrumented directory when the rule set changes with no source change - a rule-set hash in the directory name or a wrapper-side check is still needed, and [§15.5](15-final-recommendation.md) Q1's CI regression test is what catches its absence. Cost of losing cache sharing with `target/` is unmeasured (FE-10) |

---

#### E-7 · Do `extern "C"` trampolines survive `lto = true` + `codegen-units = 1` + `panic = "abort"`? *(closes FE-1)*

| | |
| --- | --- |
| **Question** | Does the trampoline symbol survive when the release profile aggressively optimizes, given it's referenced only from spliced code - exactly the shape LTO/dead-stripping targets? |
| **Environment** | Windows/MSVC, as E.1 header. `[profile.release] lto = true, codegen-units = 1, panic = "abort"`, **no added linker flags** (a first pass used `/EXPORT` anchoring and was rejected on review - anchoring a symbol proves nothing about whether it survives unanchored) |
| **Method** | `cargo clean && cargo build --release && .\target\release\app.exe` on the E-5 harness. Verified via `llvm-objdump -d` that `main` calls the trampoline directly and via the runtime probe output, since the stripped release binary carries no symbol table for `nm`/`dumpbin` to read |
| **Result** | **[Fact]** Probe fires (`[shim] enter victim::probe`, `result: 42`) from the plain, unanchored release binary. Disassembly shows the call target inlined directly into `main` under full LTO, confirming the trampoline survives not just linking but cross-crate inlining |
| **Interpretation** | The trampoline is not dead-stripped by LTO on Windows/MSVC. A separate, more consequential finding surfaced during this experiment: see E-10 below - it is not the linker that can silently break this mechanism, it's `rustc`'s own extern-crate pruning, upstream of the linker |
| **Architectural consequence** | Closes the LTO/`panic=abort` half of [R24](13-technical-risks.md)/[R25](13-technical-risks.md) for Windows. [ADR-003](17-decision-records.md) consequences updated |
| **Remaining uncertainty** | Windows/MSVC only for this specific profile combination - see E-9 for the Linux equivalent |

#### E-8 · Can a `core`-only Tier-2 future wrapper reproduce `FutureExt::with_context`'s isolation guarantee? *(closes FE-2)*

| | |
| --- | --- |
| **Question** | Can dependency-crate code, which cannot name `opentelemetry`, achieve the same context-isolation lifecycle as Tier-1's `FutureExt::with_context` over a C ABI? |
| **Environment** | Windows/MSVC, as E.1 header. `tokio` with `worker_threads = 1` to force deterministic thread sharing |
| **Method** | A hand-written `core`-only `OtelFuture<F>` wrapper: span starts on **first `poll()`** (not construction - per [§16.7](16-instrumentation-semantics.md)), `__otel_ctx_attach`/`__otel_ctx_detach` paired synchronously around each inner `poll()`, detach validates the token against the top of a thread-local stack and no-ops on mismatch rather than restoring the wrong context. Spliced into `victim`. Test: Task A starts, attaches, suspends on an unresolved `Notify` (forcing `Pending` and detach); while suspended, Task B runs on the *same* worker thread, attaches, and - while its context is live - calls a third, freshly-created span C. Task A is then released and completes. A fourth future is polled once and dropped mid-poll to test cancellation. **A first attempt at this test was rejected on review**: it started spans at construction time in `main`, before either task ran, so both spans were trivially parented to root regardless of whether attach/detach did anything - the test could not have failed |
| **Result** | **[Fact]** Span C parents to B (**positive control** - proves parenting is live and functional, not vacuous), not to A (**negative check** - proves A's context did not leak across its own suspension onto the shared thread). Span B itself parents to root, confirming A's detach fully cleared the thread before B ran. The cancelled future's span closed and exported via `Drop`. All four assertions passed against real exporter JSON, not just a summary claim |
| **Interpretation** | The mechanism is genuinely demonstrated, not merely plausible - the corrected test can distinguish "isolation works" from "isolation is entirely absent," and it distinguishes in favour of the wrapper |
| **Architectural consequence** | [R25](13-technical-risks.md) downgraded from "design, unproven" to "mechanism demonstrated, integration untested." [§16.7](16-instrumentation-semantics.md)'s first-poll-start requirement is now validated by a working implementation, not just specified |
| **Remaining uncertainty** | **Substantial - this is a hand-written demonstration, not an integration.** Not yet: spliced by the real byte-range mechanism into a real third-party crate (rather than hand-authored); tested under LTO/`panic=abort` in combination with the async wrapper specifically; tested on Linux or macOS; tested under realistic concurrent span volume (that's E-6/FE-6's territory) |

#### E-9 · Does the trampoline mechanism work on Linux, not just Windows? *(partially closes FE-7)*

| | |
| --- | --- |
| **Question** | Every experiment to date ran on Windows/MSVC. Does Mechanism 2 (no `--extern`, no `-L`) resolve and execute on Linux/ELF with GNU `ld`? |
| **Environment** | WSL2, Ubuntu 24.04 LTS, kernel `6.18.33.2-microsoft-standard-WSL2`, confirmed via `uname -a` - a real Linux kernel and a real ELF link, not Windows interop. `rustc 1.97.1` / `cargo 1.97.1`. GNU Binutils 2.46 |
| **Method** | Re-ran the E-5 harness natively under WSL2, in both debug and the same `lto=true`/`codegen-units=1`/`panic=abort` release profile as E-7 |
| **Result** | **[Fact]** `nm` shows `T __otel_span_enter` present in both the debug (`0x14460`) and release (`0x16440`) binaries. Probe fires in both (`[runtime] span enter: victim::probe`, `probe result: 42`). No `-L`/`--extern` needed in either profile |
| **Interpretation** | The trampoline mechanism is not Windows/PE-specific. Behaves identically to E-5/E-7 on ELF/GNU-ld |
| **Architectural consequence** | Closes the **Linux** leg of [R24](13-technical-risks.md). macOS/Mach-O remains open - no local environment tested it; `ld64`'s dead-strip behaviour is the specific remaining risk |
| **Remaining uncertainty** | macOS untested (needs CI, e.g. `macos-latest`). This experiment did not separately verify whether Linux exhibits the same extern-crate-pruning behaviour found in E-10 - plausible, since that pruning happens in `rustc` itself rather than the platform linker, but not directly confirmed on this platform |

#### E-10 · Does the trampoline symbol reliably reach the linker without an explicit application-side reference?

| | |
| --- | --- |
| **Question** | Surfaced while investigating E-7: is "resolves at the application's final link" ([ADR-003](17-decision-records.md)) actually automatic, or does it depend on unstated conditions? |
| **Environment** | Windows/MSVC, as E.1 header, same release profile as E-7 |
| **Method** | Tested whether `#[used] static _KEEP: unsafe extern "C" fn(...) = __otel_span_enter;` in `app`, with **no** other reference to `otel_shim`, is sufficient to get `libotel_shim.rlib` onto the link line. Compared against calling a real item in `otel_shim` (`otel_shim::init()`) from `app::main` |
| **Result** | **[Fact]** The `#[used]`-only version fails to link: `LNK2019: unresolved external symbol __otel_span_enter referenced in function victim::probe`. `libotel_shim.rlib` was never passed to `link.exe` at all - confirmed via the verbose build log, which shows `--extern otel_shim=...libotel_shim...rlib` is *not* present in the failing invocation's `rustc` command line for `app`. Calling `otel_shim::init()` (a genuine Rust item-path reference) fixes it: the rlib appears on the link line and the build succeeds |
| **Interpretation** | This is `rustc`'s own extern-crate-usage pruning - it decides which declared `--extern` crates actually reach the linker based on whether any Rust item path into that crate is referenced anywhere in the compiled graph, and this decision is made *before* linking, so it cannot be worked around at the linker level (hence `#[used]` - a linker-level anchoring mechanism - cannot fix it). `victim`'s `unsafe extern "C" { ... }` block declares a C symbol, not an item path into `otel_shim`, so it does not count |
| **Architectural consequence** | [ADR-003](17-decision-records.md) updated: the application must contain a genuine item-path reference into the runtime crate, which the generated runtime-init call already provides by design - but the tool must **preflight-check this explicitly**, since the failure otherwise surfaces as an undiagnosable linker error pointed at the wrong crate |
| **Remaining uncertainty** | Not tested whether this pruning behaviour is identical on Linux/macOS (plausible, since it is a `rustc`-level decision independent of the target linker, but not confirmed) |

#### E-11 · Does source-tree mirroring survive real multi-file crates? *(closes FE-8)*

| | |
| --- | --- |
| **Question** | E-5/E-7 used single-file crates. Does mirroring + byte-range splicing survive real published crates using `include!`, `#[path]`, `build.rs`-generated modules, and heavy `cfg` gating? |
| **Environment** | Windows/MSVC, as E.1 header. 8 published crates drawn from the local crates.io registry cache, deliberately including known hard cases |
| **Method** | For each crate: mirror the full source tree (not just the entry file) into a scratch directory; locate a non-trivial function via `syn` analysis; splice a trampoline declaration by byte range; compile the mirrored+spliced tree standalone via `cargo check --lib` against the crate's own manifest. No pruning of the crate list after seeing results |
| **Result** | **[Fact]** 5/8 compiled clean on the first pass: `bollard-buildkit-proto` (`include!` from `OUT_DIR`), `parking_lot_core` (`#[path]`, `build.rs`), `quinn-udp`, `bytes`, `aho-corasick` (32-file submodule tree). 1/8 (`crunchy`) had **zero AST functions** to splice into pre-macro-expansion - correctly classified as confirming the [§6.7](06-rust-specific-challenges.md) exclusion, not a mirroring failure. 2/8 (`serde_json`, `indexmap`) failed to compile (`E0753`) because the trampoline declaration was prepended at file-top, ahead of the file's own inner `//!` doc comments / `#![...]` attributes, which Rust's grammar requires to come first. A follow-up fix - **splicing the `extern "C"` declaration inside the target function's body instead of at file-top** - resolved both, bringing first-pass-plus-fix compatibility to 7/8 |
| **Interpretation** | Mirroring itself handles complex real-world layouts (multi-file, generated code, heavy `cfg`) without issue. The two failures were a **splicer placement bug**, not a mirroring-fidelity problem, and the fix generalizes: block-scoped declarations sidestep the inner-attribute ordering rule entirely and are legal in any function body regardless of what precedes it in the file |
| **Architectural consequence** | [ADR-002](17-decision-records.md)/[§16.3](16-instrumentation-semantics.md) updated: **trampoline declarations are block-scoped inside the target function, not file-top**, as the specified placement - not a workaround |
| **Remaining uncertainty** | `cargo check` validates parsing and type-checking, not linking - this experiment does not confirm the trampoline *resolves*, only that spliced source is grammatically and semantically valid (E-5/E-7/E-9 cover resolution, on single-file crates). 8 crates is a convenience sample, not an ecosystem-representative one - no crate here carried `#![forbid(unsafe_code)]` (see [R26](13-technical-risks.md)/FE-3, still open). No mirror-without-splice control run was performed, so mirror failures and splice failures were separated by manual follow-up rather than systematically |

---

### E.2 Source verifications - not experiments

Recorded separately because reading a primary source and running code are different grades of evidence, and conflating them is how a document starts overclaiming. Full narrative in [Appendix B](appendix-b-verification-log.md).

| # | Question | Method | Finding |
| --- | --- | --- | --- |
| V-1 | Does `otelc` publish runtime-overhead numbers? | Read `docs/benchmarking.md` in full | **No** - only a compile-time CI gate (`BENCH_MAX_OVERHEAD_PCT=150`). Later partly corrected by testimony ([Appendix D.3](appendix-d-maintainer-qa.md)): CodSpeed **compile-time** benchmarks do exist; runtime-latency benchmarks genuinely do not |
| V-2 | What is J00MZ/opentelemetry-rust-instrumentation actually doing? | Read README, `Cargo.toml`, `CONTRIBUTING.md`, `docs/how-it-works.md`; probed repo existence via the GitHub API | Aya-based; symbol scan + `rustc-demangle` + multi-return-point uprobes + JSON/DWARF/heuristic offset maps. Declares an `open-telemetry` org URL that **returns 404**. 2 of 4 claimed integrations populated |
| V-3 | Status of opentelemetry-rust#1571 | Read the closed issue, all 47 comments, and the follow-up PR | Closed 2026-03-18 as **"maintain both APIs."** Surfaced `docs/traces.md`: *"For new code, prefer the OpenTelemetry Tracing API directly"* |
| V-4 | Exact `code.*` semconv attribute names | Read the semconv registry page | **Stable** since v1.33.0: `code.function.name`, `code.file.path`, `code.line.number`, `code.column.number`, `code.stacktrace` |
| V-5 | Does OBI attach function-level uprobes to Rust code? | Triangulated OBI/Beyla/Splunk docs and a source-derived architecture reference | **No.** Go Tracer (uprobes + struct offsets) vs. Generic Tracer (socket kprobes + filters); Rust gets the latter. Later confirmed directly by an OBI maintainer ([Appendix D.4](appendix-d-maintainer-qa.md)) |
| V-6 | Is compile-time probe metadata for eBPF novel? | Read USDT documentation and the two Rust crates | **No** - USDT has done it since 2004; `oxidecomputer/usdt` (~3.8M downloads) and `cuviper/probe` (~1.8M) already emit it on stable ([Appendix C.4](appendix-c-adversarial-review.md)) |
| V-7 | Is `tokio::task::Id` stable? | Read the stabilising PRs and docs.rs | **Yes** - gated only on the `rt` feature. The original research claim that no stable task identity exists was **wrong** ([Appendix C.5](appendix-c-adversarial-review.md)) |

---

### E.3 Future experiments - **none of these have been run**

Ordered by how much the answer changes the plan. Each is scoped to be runnable in Phase 1 or as a short spike; none requires nightly.

| # | Question | Method | Blocks / informs | Priority |
| --- | --- | --- | --- | --- |
| ~~**FE-1**~~ | ~~Do injected `extern "C"` trampolines survive `lto = true` + `codegen-units = 1` + `panic = "abort"`?~~ | - | **CLOSED → [E-7](#e-7--do-extern-c-trampolines-survive-lto--true--codegen-units--1--panic--abort-closes-fe-1)**. Also surfaced [E-10](#e-10--does-the-trampoline-symbol-reliably-reach-the-linker-without-an-explicit-application-side-reference)'s extern-crate-pruning finding | - |
| ~~**FE-2**~~ | ~~Can a `core`-only spliced future wrapper reproduce `FutureExt::with_context`'s lifecycle over the C ABI, in a crate that cannot name `opentelemetry`?~~ | - | **CLOSED → [E-8](#e-8--can-a-core-only-tier-2-future-wrapper-reproduce-futureextwith_contexts-isolation-guarantee-closes-fe-2)**. Mechanism demonstrated; integration into the real splicer is not yet done - see E-8's remaining uncertainty | - |
| **FE-3** | Does `unsafe extern { safe fn … }` (Rust 1.82+) permit instrumenting a `#![forbid(unsafe_code)]` crate, or does the `unsafe_code` lint still fire? | Add the attribute to E-5's `victim`; try both declaration forms | [§16.3](16-instrumentation-semantics.md) SQ4; [R26](13-technical-risks.md). Then count the attribute's prevalence in a real dependency corpus | **Highest remaining** - a few hours; directly sizes the dependency-coverage ceiling, and untouched by any crate in E-11's sample |
| **FE-4** | Should a generated async span start at future **construction** or at **first poll**? | Construct a future, sleep 100 ms, then await it. Assert the span duration excludes the sleep | [§16.7](16-instrumentation-semantics.md) SQ1 | **Largely answered as a side effect of [E-8](#e-8--can-a-core-only-tier-2-future-wrapper-reproduce-futureextwith_contexts-isolation-guarantee-closes-fe-2)** - the wrapper implements first-poll start and it worked. The specific construction-to-first-poll delay-exclusion test (sleep before first await) has not been run separately; keep open for that narrower confirmation |
| **FE-5** | Does `FutureExt::with_context` hold up under auto-instrumentation span volume and worker-thread migration? | The [§16.16](16-instrumentation-semantics.md) oracle suite at scale: many concurrent instrumented tasks, forced migration mid-`await` | [§12.7](12-mvp-definition.md) criteria 4–5; [Appendix D.6](appendix-d-maintainer-qa.md) D-Q1; [ADR-001](17-decision-records.md) | **High** - E-8 validated the mechanism at N=2 tasks with forced interleaving; this is the same question at realistic scale and under actual migration, not a single-worker-thread proxy for it |
| **FE-6** | What is the per-span cost of the native OTel API vs. `tracing` + `tracing-opentelemetry`, and the per-poll cost of C-ABI attach/detach vs. in-crate `with_context`? | criterion microbenchmarks across [§14.2](14-evaluation-plan.md) configurations D, F, G, plus a Tier-1 vs Tier-2 comparison | D-Q2, [§16.17](16-instrumentation-semantics.md) SQ3; the empirical price of [ADR-001](17-decision-records.md) and the Tier split. **[Sharpened by E-8]** E-8 noted its global-mutex handle table was uncontended at N=2 but flagged it as a likely bottleneck at real concurrency - this experiment should include that specifically | Medium-high |
| ~~**FE-7**~~ | ~~Does the trampoline mechanism work on Linux (ELF) and macOS (Mach-O), not just Windows (PE)?~~ | - | **PARTIALLY CLOSED → [E-9](#e-9--does-the-trampoline-mechanism-work-on-linux-not-just-windows-partially-closes-fe-7)**. Linux confirmed; **macOS still open** - no local Mach-O environment, needs CI | - |
| ~~**FE-8**~~ | ~~Does source-tree mirroring survive real multi-file crates?~~ | - | **CLOSED → [E-11](#e-11--does-source-tree-mirroring-survive-real-multi-file-crates-closes-fe-8)**. 7/8 corpus compatibility after a splicer-placement fix; `cargo check`-only, not link-verified | - |
| **FE-9** | What fraction of a real crate's functions do the default exclusions remove? | `--plan-only` over the corpus; report the skip-reason distribution | [§15.5](15-final-recommendation.md) Q4 - whether the tool is useful at all | Medium-high |
| **FE-10** | What is the real build overhead, clean and incremental? | [§14.3](14-evaluation-plan.md) methodology, against the **1.5×–3×** clean-build expectation derived from `otelc`'s figures ([Appendix D.3](appendix-d-maintainer-qa.md)) | [§15.5](15-final-recommendation.md) Q5; [§15.6](15-final-recommendation.md)'s ">2× incremental" abandon signal | Medium |
| **FE-11** | Does a **body splice** survive functions rewritten by attribute macros - `#[async_trait]`, `#[tokio::main]`, `#[test]`? | Compilation matrix across the common macro combinations | [§12.9](12-mvp-definition.md) O4, [R6](13-technical-risks.md). Note this is a *different and probably easier* question than attribute-ordering, since we insert statements rather than add an attribute | Medium |
| **FE-12** | What does an always-present, runtime-disabled span cost - the configuration `STATIC_MAX_LEVEL` used to make free? | [§14.2](14-evaluation-plan.md) configuration C vs. B: CPU, allocations, and `.text` size | [R23](13-technical-risks.md), D-Q3. If material, the `--cfg` gate becomes the documented default for release builds | Medium |
| **FE-13** | *(new - [E-11](#e-11--does-source-tree-mirroring-survive-real-multi-file-crates-closes-fe-8))* Does the real byte-range splicer (not a hand-written prototype) correctly place the trampoline **inside the target function body**, and does the resulting spliced-and-linked binary actually run - across all of E-9/E-11's crates together, not each mechanism validated in isolation? | Combine E-8's wrapper, E-9's Linux build, and E-11's corpus into one pipeline: splice, mirror, build, link, run, per crate | The first true end-to-end integration test; closes the "demonstrated separately, not together" gap left by E-7/E-8/E-9/E-11 | **Highest** - this is what actually proves the MVP mechanism, as opposed to its four components individually |

**[Inference - updated]** FE-1, FE-2, and FE-7 (Linux half) are closed; FE-8 is closed for the mirroring question specifically. What replaces them at the top of the list is **FE-13**: every accepted result so far validated one mechanism at a time (LTO *or* Linux *or* async *or* mirroring), never all four together, and never through the actual production splicer rather than a hand-written stand-in. FE-3 (unsafe-forbid escape) and FE-13 (integration) are now the two that most directly gate whether Phase 1 can start generalising rather than validating.

---

← [Appendix D - Maintainer Q&A](appendix-d-maintainer-qa.md) · [Contents](../../README.md)
