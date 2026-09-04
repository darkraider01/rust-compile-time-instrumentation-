← [Rust-Specific Instrumentation Challenges](06-rust-specific-challenges.md) · [Contents](../README.md) · [Competitive / Adjacent Landscape](08-competitive-landscape.md) →

---

## 7. eBPF as a Future Extension

> ### ⛔ BRANCH CLOSED — resolved upstream, 2026-09-04
>
> **[Fact — see [Appendix D.4](appendix-d-maintainer-qa.md).]** OBI maintainer **Giuseppe Ognibene has a working prototype** of Tokio async task reconstruction and context propagation in eBPF, tracked under **OBI issue #1096 ("Rust Tokio context propagation")**, currently in final testing against task migration across worker threads and pointer reuse — the exact edge cases §7.4 and [Appendix C.9](appendix-c-adversarial-review.md) Q3 predicted would be hardest.
>
> **This resolves H2**, the load-bearing hypothesis of this entire section, and **formally triggers the [§15.6](15-final-recommendation.md) pivot condition** ("abandon the eBPF branch if OBI ships semantic Rust function-level instrumentation upstream"). Consequences:
>
> - **Architectures C and D are abandoned** ([§10](10-architecture-candidates.md)). No eBPF loader, no sidecar, no competing implementation.
> - **We collaborate rather than compete** — review Giuseppe's upstream PR when it opens and contribute the Rust async-semantics analysis in [§6.3](06-rust-specific-challenges.md) and §7.4 below, which bears directly on the edge cases still in testing.
> - **The project focuses 100% on Architecture A**, whose scope is now cleanly complementary: compile-time instrumentation serves **the platforms eBPF cannot reach at all** — macOS, Windows, unprivileged containers, non-root deployments — no matter how well #1096 works.
>
> **This section is retained as a research record**, not a plan. Its analysis of what the compiler knows that a binary does not (§7.2), of v0 mangling (§7.3), and of async state-machine structure (§7.4) is the material worth contributing upstream. Read the hypotheses in §7.5 as answered, not as pending work.

**Scope note: this section is investigation only. No eBPF work is proposed for any phase of this project.**

### 7.1 The relevant technology

| Technology | What it is | Relevance |
| --- | --- | --- |
| **[Aya](https://github.com/aya-rs/aya)** | Pure-Rust eBPF library; no libbpf/BCC dependency, uses only `libc` for syscalls. Async support for tokio and async-std | Historical context: was the natural choice if we ever wrote eBPF from Rust; out of scope for active architecture |
| **uprobes / uretprobes** | Kernel mechanism to trap at a user-space instruction address | The attachment mechanism |
| **BTF** | Compact type information format, primarily for kernel types; enables CO-RE | Kernel-side portability of our BPF programs |
| **DWARF** | Full debug information — types, variable locations, line tables, inlining records | The only reliable source of Rust type/source information in a binary |
| **ELF symbol tables** | `.symtab` (strippable) and `.dynsym` (dynamic) | The usual, fragile, source of function addresses |
| **Build ID** | `.note.gnu.build-id`, a hash uniquely identifying a build | Was proposed as the join key for a bespoke metadata sidecar; superseded — see the USDT row below |
| **v0 symbol mangling** | Rust's own mangling scheme | See §7.3 |
| **USDT (`.note.stapstd`)** *(added after the adversarial review round — [Appendix C.4](appendix-c-adversarial-review.md))* | Userland Statically Defined Tracing: compile-time probe metadata (name, argument locations) embedded directly in ELF notes. Two decades old (Sun DTrace, 2004), consumed natively by `bpftrace`/`libbpf`/Aya. Stable-Rust crates already emit it: [`oxidecomputer/usdt`](https://github.com/oxidecomputer/usdt) (~3.8M downloads) and [`cuviper/probe`](https://github.com/cuviper/probe-rs) (~1.8M) | **Supersedes the bespoke `instrument-metadata.json`/build-ID sidecar in Architecture C.** Solves "probe name + argument locations" today, on stable Rust, with a `nop`-when-detached cost. Does **not** solve async state-machine structure — see §7.4 |

**[Fact]** Aya's `UProbe::attach` signature is:

```rust
pub fn attach<'a, T: AsRef<Path>, Point: Into<UProbeAttachPoint<'a>>>(
    &mut self, point: Point, target: T, scope: UProbeScope,
) -> Result<UProbeLinkId, ProgramError>
```

An attach point is a symbol (optionally plus an offset added to the function's address) or an absolute object-file offset; `UProbeAttachLocation::from_virtual_address()` converts an ELF virtual address into one. `target` is a path to a binary or shared library, or a library name. `uprobe` attaches at the function's start address; `uretprobe` at its return address. Attach cookies (kernel 5.15+) are available to the BPF program via `bpf_get_attach_cookie()`.

**[Inference]** The attach cookie was considered architecturally significant in our initial investigation: it lets a *userspace* loader associate arbitrary out-of-band data (e.g. "this probe is span-site #47, name `handle_request`, kind `server`") with a probe, so the BPF program does not need to encode that knowledge itself. In the closed Architecture C candidate, that was the considered delivery vehicle for compiler-generated metadata.

### 7.2 The central question

> **What can the Rust compiler know at compile time that an eBPF runtime cannot reliably infer from the final binary?**

**[Fact] — things the compiler knows and the binary does not retain (or retains only unreliably):**

| Knowledge | Compiler | Binary |
| --- | --- | --- |
| Which functions are `async fn` vs. ordinary | Known exactly (HIR desugaring) | Not recoverable in general. The state machine is an anonymous type; the relationship between `foo` and `foo::{{closure}}`'s poll is a naming convention, not a guarantee |
| Which coroutine state variant corresponds to which `.await` in the source | Known exactly (`StateTransform` builds the mapping) | Not present anywhere |
| That two symbols are instantiations of one generic definition | Known exactly | Recoverable from v0 mangling by demangling, but only for symbols that survive |
| Whether a function was inlined | Known after codegen | DWARF records inlining (`DW_TAG_inlined_subroutine`) if debuginfo is on; absent otherwise |
| Trait ↔ impl relationships; which `impl` a `dyn` call can reach | Known | Erased |
| The semantic role of a function ("this is an HTTP handler," "this is a DB query") | Derivable from the crate/type it belongs to | Only guessable from symbol names |
| Function argument names and types | Known exactly | DWARF only, and argument *locations* at a given PC are notoriously fragile in optimized code |
| Source file and line for a span | Known exactly | DWARF line table only |
| The exact set of monomorphized instantiations that exist | Known at monomorphization | Requires enumerating all symbols |
| Whether a function can panic / unwind | Known (approximately) | Not present |
| Crate identity and version of the code a symbol came from | Known exactly | v0 mangling encodes a crate disambiguator hash, not a human-readable version |

**[Fact] — things eBPF knows and the compiler does not:**

- Actual runtime values, arguments, and return values on real traffic.
- Actual thread/CPU/scheduling behaviour.
- Kernel-side events: syscalls, socket activity, TLS, network timing.
- Which code paths actually execute, and how often.
- It works on binaries you did not build.

**[Inference]** The two are genuinely complementary, and the complementarity is sharper for Rust than for Go: Go's runtime provides a stable, introspectable structure (goroutines, `g` struct offsets) that eBPF can exploit — which is why OBI gives Go memory-level context propagation and everyone else network-level only **[Fact]**. Rust has *no runtime*, so eBPF has nothing analogous to latch onto. Compiler-generated metadata is the most obvious candidate for filling that specific void.

### 7.3 Symbol mangling: a recent and material change

**[Fact]** Rust's v0 mangling scheme is now the **default on stable**. `-C symbol-mangling-version=v0` has been available since 1.59; it became the stable default in Rust 1.97.0 (released 2026-07-09) via [PR #151994](https://github.com/rust-lang/rust/pull/151994), and `legacy` is now nightly-only. **[Fact]** v0 encodes generic parameters reversibly, has a consistent specification, and restricts symbols to `[A-Za-z0-9_]`, explicitly to improve compatibility with debuggers and profilers. Symbols can be decoded with `rustfilt`.

**[Inference — this weakens part of our hypothesis, and we should say so]** A substantial slice of the "eBPF cannot understand Rust symbols" argument was really "legacy mangling was lossy and ambiguous." With v0 as the stable default, generic instantiations, crate identity, and path structure are all recoverable by demangling. Any historical justification for compiler metadata must be careful not to claim credit for problems that v0 already solved (and with ADR-005 closing the eBPF branch, no Phase 3 metadata or eBPF work will take place). What v0 does **not** give you: async structure, trait↔impl relationships, semantic roles, inlining, or anything at all if the binary is stripped.

### 7.4 Could compiler-generated metadata improve eBPF instrumentation?

Evaluated claim by claim. All of the following are **[Hypothesis]** unless marked otherwise.

**More precise — likely yes.**
Compiler-emitted per-function records (symbol name, mangled name, source location, function kind: `sync`/`async-outer`/`async-poll`/`closure`/`trait-impl`, plus the generic-definition it instantiates) would let a loader attach probes to exactly the right symbols with no guessing. **[Fact]** supporting this: uprobe attachment already accepts a symbol plus offset, so we would only be supplying a better-curated list, not a new mechanism. **Risk, sharpened after the adversarial review round:** the *delivery mechanism* for this precision — probe name plus argument locations embedded in the binary — is exactly what USDT already provides on stable Rust today, via `oxidecomputer/usdt`/`cuviper/probe` ([Appendix C.4](appendix-c-adversarial-review.md)). If v0 demangling *plus* USDT already yields most of the ordinary precision gain, the specifically compiler-derived marginal value narrows to what neither can express: async state-machine structure (function kind `async-poll` vs. `async-outer`, and the state-variant↔`.await` mapping) — see "More semantic" below.

**Easier to configure — likely yes, and this may be the strongest claim.**
Today configuring uprobe-based Rust instrumentation means writing symbol patterns. With metadata, a user could write `instrument: crate=my_service, kind=async, module=handlers::*` and the loader would resolve it to addresses. **[Inference]** This is a user-experience improvement, not a capability improvement — but it is the difference between a tool people can use and one they cannot.

**More semantic — yes, and this is the part symbols genuinely cannot supply.**
"This symbol is the poll function of the async fn `handle_checkout` declared at `src/handlers.rs:42`, which is an axum handler for `POST /checkout`" is not derivable from any binary. It is derivable at compile time. This is the clearest genuine gap.

**Less dependent on fragile symbol discovery — largely superseded by USDT, with the async-specific part still open.**
**[Revised — see [Appendix C.4](appendix-c-adversarial-review.md)]** The original framing proposed a bespoke JSON sidecar recording offsets from the build ID's load base, with the build ID as the join key against binary drift. This is no longer the right design: USDT solves exactly this problem — probes are embedded in the binary itself (no separate file to drift, no join key needed) and are already stripped-binary-friendly by design. **[Fact]** Aya, `bpftrace`, and `libbpf` all consume USDT notes natively. What USDT does not solve, and what remains a genuinely open metadata-design question, is encoding the *async-specific* fields (function kind, state-variant↔`.await` mapping) inside or alongside a USDT probe's payload — USDT's argument-location format was not designed for this and may need a companion section. **[Open question]** How PIE/ASLR base resolution and post-build modification (`strip`, `objcopy`) interact with USDT notes specifically, as opposed to the standard function symbol table, has not been tested.

**Lower overhead — unclear, do not claim it.**
**[Hypothesis]** Attaching 30 well-chosen probes instead of 3000 blanket ones is obviously cheaper. But that is a consequence of better *selection*, achievable by any means. Metadata does not make an individual uprobe cheaper. **[Fact]** uprobes have a real per-hit cost (a trap into the kernel); nothing about metadata changes that. Any overhead claim must be measured, not asserted.

**Better for async Rust — plausibly the single strongest case, and the least proven.**
The information an eBPF tool most needs for async Rust — "these poll invocations all belong to one logical operation," "this state variant means the task is suspended at this `.await`," "this task was spawned from that one" — is exactly what `StateTransform` computes and then discards **[Fact, that it computes it]**. If the compiler emitted the state-variant ↔ source-`.await` mapping, an eBPF tool could in principle reconstruct logical async spans from poll events. **[Hypothesis, revised — see [Appendix C.5](appendix-c-adversarial-review.md)]** Whether this is *practically* reconstructible at runtime requires a stable identity to correlate polls belonging to the same task. The original research claimed no such identity exists; this was wrong — **[Fact]** `tokio::task::Id` has been stable since PRs [#6793](https://github.com/tokio-rs/tokio/pull/6793)/[#6891](https://github.com/tokio-rs/tokio/pull/6891) and is gated only on the `rt` feature. The real open question is narrower: `task::Id` is a stable *Rust API*, not a stable *external memory layout* — a uprobe attached from outside the process cannot call `task::Id`, it must recover an equivalent identity (e.g. the `&Task` pointer passed in a register at the executor's poll entry) directly from memory, which depends on Tokio's internal struct layout and is exactly the kind of fragile, version-dependent offset-tracking that [J00MZ already documents fighting](04-rust-instrumentation-landscape.md) for other struct layouts. So the question is not "does an identity exist" (it does) but "can an external observer read it reliably across Tokio versions without compiler assistance" — which is unknown and is still the question that determines whether the whole eBPF branch is worth pursuing.

**[Fact — new evidence from the verification pass, see [§4.5](04-rust-instrumentation-landscape.md) and [Appendix B](appendix-b-verification-log.md) item 3]** This is not a purely theoretical question. [J00MZ/opentelemetry-rust-instrumentation](https://github.com/J00MZ/opentelemetry-rust-instrumentation) is already attempting exactly this, without compiler assistance: its documentation states the strategy as "instrument at the executor level and track task contexts to maintain proper span hierarchies." No accuracy metrics are published, and the project has not shipped context propagation. This is the closest available real-world data point on H2 — it shows someone is actively trying to solve the reconstruction problem with runtime heuristics alone, and has not yet published evidence that it works well. It neither confirms nor refutes H2, but it materially raises our confidence that H2 is a genuinely open, currently-being-worked-on problem rather than an already-solved one we would be redundantly re-attempting.

### 7.5 Established fact vs. hypothesis: a clean split

**Established facts:**
- **[Confirmed twice — in verification, then directly by OBI maintainer Nikola Grcevski ([Appendix D.4](appendix-d-maintainer-qa.md))]** OBI's Rust support is *not* "Go with weaker propagation" — it is architecturally the Generic Tracer path (kprobes on kernel socket syscalls + socket filters), the same path used for any language without a bespoke tracer. OBI attaches **zero function-level uprobes into Rust application code**. Only Go gets a dedicated tracer with library-level uprobes and struct-offset resolution. This is the *current* state; #1096 is the work that changes it.
- Aya can attach uprobes by symbol or by offset, and supports attach cookies for out-of-band data.
- v0 mangling is now the stable default and encodes generics reversibly.
- rustc's `StateTransform` computes an exact mapping from `.await` points to coroutine state variants, and this mapping does not survive into the binary.
- An early-stage third-party project ([J00MZ/opentelemetry-rust-instrumentation](https://github.com/J00MZ/opentelemetry-rust-instrumentation), explicitly modeled on the real `open-telemetry/opentelemetry-go-instrumentation` but not itself an OTel-org project) is already attempting uprobe-based Rust auto-instrumentation via symbol demangling and multi-return-point uprobes, and is already attempting an executor-level heuristic for async span reconstruction — i.e. attempting H2 without compiler help.

**Hypotheses — all closed by [Appendix D.4](appendix-d-maintainer-qa.md); none will be validated by this project:**

| # | Hypothesis | Disposition |
| --- | --- | --- |
| H1 | Compiler-emitted function metadata materially improves probe selection precision *beyond what v0 demangling already provides* | **Not ours to answer.** The baseline was already narrowed by verification to "does it beat symbol-based selection as J00MZ does it," and by [Appendix C.4](appendix-c-adversarial-review.md) to "beyond what USDT already provides." With the branch closed, this is a question for OBI's maintainers, and the analysis in §7.2/§7.3 is what we contribute to it |
| H2 | Logical async spans can be reconstructed at runtime from poll-level uprobe events plus compile-time state-machine metadata | **RESOLVED UPSTREAM.** Giuseppe Ognibene's prototype (OBI #1096) reconstructs Tokio async task context in eBPF and is in final testing. The identity/correlation problem this section spent most of its length on — can an external observer track a task across polls and thread migrations — has been answered by someone building it inside OBI ([Appendix D.4](appendix-d-maintainer-qa.md)) |
| H3 | Offset-based probe attachment via build-ID-keyed metadata works reliably on stripped, PIE, optimized release binaries | **Moot twice over.** [Appendix C.4](appendix-c-adversarial-review.md) already replaced the build-ID-keyed sidecar with ELF-native USDT notes; the branch closure removes the remaining question |
| H4 | The combined overhead of a metadata-guided eBPF approach is lower than compile-time instrumentation for equivalent span coverage | **Not ours to answer.** Once both approaches exist independently — #1096 upstream, Architecture A here — this becomes a comparison someone can run, not a hypothesis either project depends on |

**[Inference]** H2 was correctly identified as the load-bearing one, and the reasoning held: it decided the branch. It simply decided it by being answered upstream rather than by being tested here. That is the outcome to prefer — the question was real, and it got resolved before we spent a phase on it.

**Open questions — retained as a record; none block this project:**
- ~~How does the J00MZ project actually resolve axum/hyper handlers from symbols, and where does it fail?~~ **Partially answered in verification** (§4.5): symbol-table scan + `rustc-demangle` + per-library pattern matching + multi-return-point uprobes + JSON/DWARF/heuristic struct-offset tracking. Failure modes (accuracy under optimization, version drift) are not published and remain open.
- ~~Is there a stable task identity in Tokio observable from eBPF?~~ **Partially answered.** `tokio::task::Id` is a stable *Rust API* (Appendix C.5) — the identity exists. What remains open is whether an *external* eBPF observer can reliably recover an equivalent identity from memory (e.g. a `&Task` pointer at a known register/offset) across Tokio versions without compiler assistance. If not, H2 is likely false.
- ~~Does OBI have an extension point that would accept externally supplied instrumentation metadata, or would this require a fork?~~ **Overtaken by events.** The question presumed we would be feeding metadata into OBI from outside. With #1096 building Rust async support inside OBI, the productive route is contribution to that work, not an extension point for ours ([Appendix D.4](appendix-d-maintainer-qa.md)).
---

---

← [Rust-Specific Instrumentation Challenges](06-rust-specific-challenges.md) · [Contents](../README.md) · [Competitive / Adjacent Landscape](08-competitive-landscape.md) →
