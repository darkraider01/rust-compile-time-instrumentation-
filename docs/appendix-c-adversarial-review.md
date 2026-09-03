← [Appendix B — Verification Log](appendix-b-verification-log.md) · [Contents](../README.md)

---

## Appendix C — Adversarial review round (2026-09-04)

An independent agent ("Antigravity") reviewed the Phase 0 research and argued that Architecture A contains *"a fatal structural flaw"* that renders it *"impossible on stable Rust."* This appendix records the disputed claims, the experiments run to settle them, and what changed as a result.

**Method:** every disputed claim was checked against a primary source or, where the documentation was silent or ambiguous, settled by building and running code on the local toolchain (`rustc 1.97.1` / `cargo 1.97.1`, Windows). The review was not accepted on authority, and several of its claims did not survive.

---

### C.1 Scorecard

| Review claim | Verdict | Basis |
| --- | --- | --- |
| Architecture A is "structurally blocked on stable Rust" (headline claim) | **Refuted** | Experiment C.2 — works two different ways |
| Cargo won't pass `--extern tracing` → `E0433` | **Refuted** | The wrapper appends the flag itself; dependency compiled clean |
| Topological scheduling means the runtime `.rlib` won't exist | **Refuted** | Pre-built out-of-band in Phase 1; never a Cargo DAG node |
| `--extern=force:` is nightly, so force-linking is impossible | **Conflation** | `force:` is only needed when source does *not* reference the crate (the MIR case, §3.2.5) |
| `tracing` spans cannot express OTel span kind / status / remote parent | **Mostly refuted** | `otel.kind`, `otel.status_code`, `otel.name` are constants in `tracing-opentelemetry/src/layer.rs`; only **links** is a genuine gap |
| `dalibo/hud` as eBPF Tokio precedent | **Hallucinated citation** — repository returns HTTP 404 | GitHub API |
| `cargo-trace` as relevant competitor | **Overstated** — last pushed 2021-03-04, 40 stars | GitHub API |
| fastrace is "10–100× faster than `tracing`" | **Real project, self-reported figure** | That string is the project's own tagline; they do publish benchmarks |
| `cargo-mutants` splices bytes rather than pretty-printing | **Confirmed — adopted** | mutants.rs primary documentation |
| USDT already provides compile-time ELF metadata for eBPF | **Confirmed — adopted** | `usdt` v0.6.0, 3.8M downloads, stable since inline `asm!` in Rust 1.59 |
| `tokio::task::Id` is stable | **Confirmed — original claim was wrong** | PRs #6793 / #6891; docs.rs shows only the `rt` feature gate |
| `RUSTFLAGS` cache-busting is a blunderbuss | **Confirmed — mitigation replaced** | Cargo config precedence: env clobbers config without merging |
| `STATIC_MAX_LEVEL` is not a per-tool kill switch | **Confirmed** | It is global and additive across the dependency graph |

Two process notes on the review itself, recorded because they affect how much weight to give its unverified assertions: it cited a repository that does not exist, and it presented a project's marketing tagline as a benchmark result. Its concrete corrections are valuable and several are adopted below; its stated confidence was not calibrated to its evidence.

---

### C.2 Experiment: can a stable-Rust wrapper instrument a third-party dependency?

This is the decisive question — the review called it the project's fatal flaw, and [§9.5](09-gap-analysis.md) calls dependency coverage *"the single test that separates a real tool from a wrapper."*

**Setup.** Three crates: `otel_shim` (a runtime, built standalone), `victim` (simulating a third-party dependency — it does **not** declare `otel_shim` in its `Cargo.toml`), and `app` (depends only on `victim`). A native `RUSTC_WRAPPER` binary intercepts `victim`'s compilation, rewrites its source to add an instrumented function, and appends flags to the `rustc` argv.

**Mechanism 1 — inject an undeclared crate dependency.** Wrapper appends `-L dependency=<shim_dir>` and `--extern otel_shim=<path>`:

```
   Compiling victim v0.1.0
[wrapper] injecting shim into `victim`
   Compiling app v0.1.0
    Finished dev profile
     Running target\debug\app.exe
42
[shim] enter victim::probe
```

`victim` compiled and the injected probe executed. **No `E0433`.** The review's premise (Cargo does not pass `--extern` for undeclared deps) is correct; its conclusion does not follow, because the wrapper rewrites the argv rather than passively forwarding it.

**A real constraint the review did not anticipate:** the first attempt failed at the *downstream* crate with `E0463: can't find crate for otel_shim which victim depends on`. The injected dependency propagates into crate metadata, so **every** downstream consumer needs `-L` as well. One line of wrapper logic; not architectural.

**Mechanism 2 — extern `"C"` trampoline (no Cargo graph involvement at all).** Wrapper injects only a declaration, and appends **no flags whatsoever**:

```rust
unsafe extern "C" { fn __otel_span_enter(name: *const u8, len: usize); }
```

```
   Compiling victim v0.1.0
[wrapper] injected extern "C" trampoline into `victim` (no --extern, no -L)
   Compiling app v0.1.0
    Finished dev profile
     Running target\debug\app.exe
42
[runtime] span enter: victim::probe
```

The symbol resolves at final link from a runtime the *application* declares. **[Fact]** Both mechanisms work on stable Rust with no `-Zunstable-options`.

**Adopted:** Mechanism 2. It structurally cannot produce the duplicate-crate or dependency-cycle hazards the review raised against Mechanism 1, needs no `-L` propagation, and mirrors `otelc`'s actual trampoline design ([§2.5](02-otelc-go.md)). The review's "Path 2" suggestion was right even though its argument for why Path 1 is impossible was wrong.

---

### C.3 Experiment: cache isolation without `RUSTFLAGS`

[Appendix B item 2](appendix-b-verification-log.md) established that `RUSTC_WRAPPER` is not in Cargo's rebuild fingerprint, and proposed a `RUSTFLAGS`-hash cache-buster. The review correctly identified that as destructive: `RUSTFLAGS` is global (evicting every crate including build scripts), and the environment variable **clobbers** `.cargo/config.toml` rustflags rather than merging with them — stripping any user-configured sanitizers, target flags, or link arguments.

**Replacement, tested:** a separate `--target-dir`.

```
cargo build --target-dir target/instrumented   # with RUSTC_WRAPPER set
```

The wrapper was invoked for every crate (no stale-artifact reuse, because the isolated directory contains no uninstrumented artifacts), and the default `target/` cache was left intact. This solves cache isolation *and* the stale-artifact problem together, without touching user flags. **[Fact]**

---

### C.4 USDT: the metadata mechanism was not novel

[§7.4](07-ebpf-future.md) and [§9.3](09-gap-analysis.md) described compiler-emitted metadata for eBPF as the genuinely unexplored idea. This was wrong as stated.

**[Fact]** Userland Statically Defined Tracing (USDT) has embedded probe metadata — location, argument types, register locations — directly into ELF notes since Sun DTrace (2004), and is consumed natively by SystemTap, `bpftrace`, `libbpf` (`bpf_program__attach_usdt`) and Aya. In Rust specifically, two mature crates already do this on **stable**:

- [`oxidecomputer/usdt`](https://github.com/oxidecomputer/usdt) — v0.6.0, ~3.8M downloads. Requires inline `asm!`, stable since Rust 1.59; x86-64 Linux supported via SystemTap v3 probes.
- [`cuviper/probe`](https://github.com/cuviper/probe-rs) — v0.5.2, ~1.8M downloads.

Probes compile to a single `nop` when no tracer is attached, and carry no out-of-band file to drift.

**What this kills:** Architecture C's bespoke `instrument-metadata.json` sidecar joined by GNU build ID. Inventing a private sidecar format when a universal ELF-native one exists was a design error.

**What survives — and this is the narrowed novelty claim:** USDT notes encode *probe name and argument locations*. They do **not** encode Rust async state-machine structure — the `.await`-point ↔ coroutine-state-variant mapping that `rustc`'s `StateTransform` computes and then discards ([§3.2.5](03-rust-compiler-pipeline.md), [§6.3](06-rust-specific-challenges.md)). That specific *content* remains unclaimed. The contribution narrows from **"compiler metadata for eBPF"** (decades old) to **"async-structure metadata for eBPF"** (still open).

---

### C.5 Tokio task identity: original claim was wrong, conclusion partly survives

**[Fact]** `tokio::task::Id` is stable — stabilized via PRs [#6793](https://github.com/tokio-rs/tokio/pull/6793) and [#6891](https://github.com/tokio-rs/tokio/pull/6891); docs.rs shows only an `rt` feature gate, no `tokio_unstable`. The original research asserted Tokio "does not provide" and "does not expose to eBPF" a stable task identity, and made that the kill-switch for the entire eBPF branch in §15.6. **That claim is withdrawn.**

**But the review's supporting argument is also wrong.** It cited `tokio-console` as proof that eBPF can reconstruct async execution. `tokio-console` works via **in-process** `tracing` instrumentation compiled into Tokio under `--cfg tokio_unstable` — it is not an external observer and demonstrates nothing about eBPF's reach. That `task::Id` is a stable *Rust API* says nothing about whether an out-of-process observer can read it: eBPF must extract it from the task struct at a known offset, which is exactly the struct-layout fragility that [J00MZ documents](04-rust-instrumentation-landscape.md) (JSON offset maps → DWARF → heuristics).

**What the review got right:** its *second* argument — that the task identity available to a uprobe is a concrete `&Task` pointer in a register at the executor's poll entry — is sound and better than its first. Accepted as a viable correlation key, with pointer-reuse-after-free as an untested hazard.

**Net effect on H2:** it moves from *"probably impossible, no identity exists"* to *"plausible, but dependent on runtime memory layout."* And the layout dependency is itself the argument for compiler-emitted metadata. **[Hypothesis — materially strengthened, still untested.]**

---

### C.6 Surgical splicing replaces pretty-printing

**[Fact]** From cargo-mutants' own documentation: *"The file is parsed using the syn crate, but mutations are applied textually, rather than to the token stream, so that unmutated code retains its prior formatting, comments, line numbers, etc."*

[Appendix B item 7](appendix-b-verification-log.md) measured `syn`+`prettyplease`'s damage correctly (all non-doc comments destroyed, whole-file reformat) but drew the wrong conclusion — it framed this as a cost to disclose, and treated `ra_ap_syntax` as the only alternative. The correct answer is a third option the original research missed:

1. `syn::parse_file` for **analysis only** — locate targets, read attributes, check exclusions.
2. Take `span().byte_range()` of the matched item.
3. Splice the attribute or trampoline into the **original UTF-8 buffer** at that offset.

Comments, formatting, and line numbers all survive byte-for-byte outside the insertion point. This substantially defuses [R11](13-technical-risks.md) and removes the need to evaluate `ra_ap_syntax` at all for the MVP.

---

### C.7 Projects the original landscape missed

| Project | Status (verified) | Relevance |
| --- | --- | --- |
| [`fastrace`](https://github.com/fast/fastrace) | v0.7.19, ~6.8M downloads, 1131 stars, pushed 2026-07-31 | A genuine **third option** for the §5.4 emitter decision that the original two-way analysis missed. Has `fastrace-opentelemetry` (OTLP export) and `fastrace-tracing` (captures spans from `tracing`-instrumented libraries). Its "10–100× faster" figure is its own tagline; benchmarks are published in-repo but are self-reported and not for our workload |
| [`oxidecomputer/usdt`](https://github.com/oxidecomputer/usdt) | v0.6.0, ~3.8M downloads | See C.4 — replaces the sidecar design |
| [`cuviper/probe`](https://github.com/cuviper/probe-rs) | v0.5.2, ~1.8M downloads | Alternative static probe crate |
| [`cargo-mutants`](https://github.com/sourcefrog/cargo-mutants) | Active | Source of the splicing technique (C.6) |
| [`dvc94ch/cargo-trace`](https://github.com/dvc94ch/cargo-trace) | **Dormant** — 40 stars, last pushed 2021-03-04 | Marginal. Early prior art for uprobe attachment on Rust binaries; the review overstated it as a live competitor |
| `dalibo/hud` | **Does not exist** (HTTP 404) | Cited by the review as eBPF Tokio scheduler monitoring; no such repository |

---

### C.8 What changed as a result

| # | Change | Driver |
| --- | --- | --- |
| 1 | Inject extern `"C"` trampolines, not crate dependencies | C.2 — avoids cycle/duplicate hazards entirely |
| 2 | Byte-range splicing, not `syn`→`prettyplease` | C.6 — preserves comments and line numbers |
| 3 | Isolated `--target-dir`, not `RUSTFLAGS` cache-busting | C.3 — no workspace-wide eviction, no config clobbering |
| 4 | USDT notes, not a bespoke JSON sidecar | C.4 — universal, ELF-native, no metadata drift |
| 5 | Pull a one-dependency slice into the MVP | C.2 — the mechanism is proven, so deferring it only defers the differentiator |
| 6 | Novelty claim narrowed to *async-structure* metadata | C.4 — the mechanism is not novel; the content still is |
| 7 | §15.6 abandon condition on task identity rewritten | C.5 — the premise was false |
| 8 | `STATIC_MAX_LEVEL` demoted from "free kill switch" | Review — it is global and disables users' own spans too |

---

← [Appendix B — Verification Log](appendix-b-verification-log.md) · [Contents](../README.md)
