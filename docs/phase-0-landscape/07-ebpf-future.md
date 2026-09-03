← [Rust-Specific Instrumentation Challenges](06-rust-specific-challenges.md) · [Contents](README.md) · [Competitive / Adjacent Landscape](08-competitive-landscape.md) →

---

## 7. eBPF as a Future Extension


**Scope note: this section is investigation only. No eBPF work is proposed for Phase 1 or Phase 2.**

### 7.1 The relevant technology

| Technology | What it is | Relevance |
| --- | --- | --- |
| **[Aya](https://github.com/aya-rs/aya)** | Pure-Rust eBPF library; no libbpf/BCC dependency, uses only `libc` for syscalls. Async support for tokio and async-std | The natural choice if we ever write eBPF from Rust |
| **uprobes / uretprobes** | Kernel mechanism to trap at a user-space instruction address | The attachment mechanism |
| **BTF** | Compact type information format, primarily for kernel types; enables CO-RE | Kernel-side portability of our BPF programs |
| **DWARF** | Full debug information — types, variable locations, line tables, inlining records | The only reliable source of Rust type/source information in a binary |
| **ELF symbol tables** | `.symtab` (strippable) and `.dynsym` (dynamic) | The usual, fragile, source of function addresses |
| **Build ID** | `.note.gnu.build-id`, a hash uniquely identifying a build | The correct join key between a binary and out-of-band metadata |
| **v0 symbol mangling** | Rust's own mangling scheme | See §7.3 |

**[Fact]** Aya's `UProbe::attach` signature is:

```rust
pub fn attach<'a, T: AsRef<Path>, Point: Into<UProbeAttachPoint<'a>>>(
    &mut self, point: Point, target: T, scope: UProbeScope,
) -> Result<UProbeLinkId, ProgramError>
```

An attach point is a symbol (optionally plus an offset added to the function's address) or an absolute object-file offset; `UProbeAttachLocation::from_virtual_address()` converts an ELF virtual address into one. `target` is a path to a binary or shared library, or a library name. `uprobe` attaches at the function's start address; `uretprobe` at its return address. Attach cookies (kernel 5.15+) are available to the BPF program via `bpf_get_attach_cookie()`.

**[Inference]** The attach cookie is architecturally significant for us: it lets a *userspace* loader associate arbitrary out-of-band data (e.g. "this probe is span-site #47, name `handle_request`, kind `server`") with a probe, so the BPF program does not need to encode that knowledge itself. That is the natural delivery vehicle for compiler-generated metadata.

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

**[Inference — this weakens part of our hypothesis, and we should say so]** A substantial slice of the "eBPF cannot understand Rust symbols" argument was really "legacy mangling was lossy and ambiguous." With v0 as the stable default, generic instantiations, crate identity, and path structure are all recoverable by demangling. Any Phase 3 justification for compiler metadata must be careful not to claim credit for problems that v0 already solved. What v0 does **not** give you: async structure, trait↔impl relationships, semantic roles, inlining, or anything at all if the binary is stripped.

### 7.4 Could compiler-generated metadata improve eBPF instrumentation?

Evaluated claim by claim. All of the following are **[Hypothesis]** unless marked otherwise.

**More precise — likely yes.**
Compiler-emitted per-function records (symbol name, mangled name, source location, function kind: `sync`/`async-outer`/`async-poll`/`closure`/`trait-impl`, plus the generic-definition it instantiates) would let a loader attach probes to exactly the right symbols with no guessing. **[Fact]** supporting this: uprobe attachment already accepts a symbol plus offset, so we would only be supplying a better-curated list, not a new mechanism. **Risk:** if v0 demangling already yields most of this, the marginal gain is small.

**Easier to configure — likely yes, and this may be the strongest claim.**
Today configuring uprobe-based Rust instrumentation means writing symbol patterns. With metadata, a user could write `instrument: crate=my_service, kind=async, module=handlers::*` and the loader would resolve it to addresses. **[Inference]** This is a user-experience improvement, not a capability improvement — but it is the difference between a tool people can use and one they cannot.

**More semantic — yes, and this is the part symbols genuinely cannot supply.**
"This symbol is the poll function of the async fn `handle_checkout` declared at `src/handlers.rs:42`, which is an axum handler for `POST /checkout`" is not derivable from any binary. It is derivable at compile time. This is the clearest genuine gap.

**Less dependent on fragile symbol discovery — partially, with an important caveat.**
Metadata can record *offsets from the build ID's load base* rather than symbol names, so probes could attach to stripped binaries. **[Fact]** Aya supports absolute object-file offsets and `from_virtual_address()`. **[Open question]** How this interacts with PIE/ASLR base resolution, with `-C prefer-dynamic`, and with binaries modified after build (e.g. `strip`, `objcopy`, UPX) is unknown and must be tested. **Caveat:** we would be replacing dependence on the symbol table with dependence on our own sidecar metadata file staying in sync with the binary — which is what the build ID join key is for, but it is a real new failure mode (metadata drift), not a free win.

**Lower overhead — unclear, do not claim it.**
**[Hypothesis]** Attaching 30 well-chosen probes instead of 3000 blanket ones is obviously cheaper. But that is a consequence of better *selection*, achievable by any means. Metadata does not make an individual uprobe cheaper. **[Fact]** uprobes have a real per-hit cost (a trap into the kernel); nothing about metadata changes that. Any overhead claim must be measured, not asserted.

**Better for async Rust — plausibly the single strongest case, and the least proven.**
The information an eBPF tool most needs for async Rust — "these poll invocations all belong to one logical operation," "this state variant means the task is suspended at this `.await`," "this task was spawned from that one" — is exactly what `StateTransform` computes and then discards **[Fact, that it computes it]**. If the compiler emitted the state-variant ↔ source-`.await` mapping, an eBPF tool could in principle reconstruct logical async spans from poll events. **[Hypothesis]** Whether this is *practically* reconstructible at runtime — you would need a stable task identity to correlate polls, which Rust does not provide and Tokio does not expose to eBPF — is unknown and is the question that determines whether the whole eBPF branch of this project is worth pursuing.

**[Fact — new evidence from the verification pass, see [§4.5](04-rust-instrumentation-landscape.md) and [Appendix B](appendix-b-verification-log.md) item 3]** This is not a purely theoretical question. [J00MZ/opentelemetry-rust-instrumentation](https://github.com/J00MZ/opentelemetry-rust-instrumentation) is already attempting exactly this, without compiler assistance: its documentation states the strategy as "instrument at the executor level and track task contexts to maintain proper span hierarchies." No accuracy metrics are published, and the project has not shipped context propagation. This is the closest available real-world data point on H2 — it shows someone is actively trying to solve the reconstruction problem with runtime heuristics alone, and has not yet published evidence that it works well. It neither confirms nor refutes H2, but it materially raises our confidence that H2 is a genuinely open, currently-being-worked-on problem rather than an already-solved one we would be redundantly re-attempting.

### 7.5 Established fact vs. hypothesis: a clean split

**Established facts:**
- **[Confirmed in verification, previously stated more weakly]** OBI's Rust support is *not* "Go with weaker propagation" — it is architecturally the Generic Tracer path (kprobes on kernel socket syscalls + socket filters), the same path used for any language without a bespoke tracer. OBI attaches **zero function-level uprobes into Rust application code**. Only Go gets a dedicated tracer with library-level uprobes and struct-offset resolution.
- Aya can attach uprobes by symbol or by offset, and supports attach cookies for out-of-band data.
- v0 mangling is now the stable default and encodes generics reversibly.
- rustc's `StateTransform` computes an exact mapping from `.await` points to coroutine state variants, and this mapping does not survive into the binary.
- An early-stage third-party project ([J00MZ/opentelemetry-rust-instrumentation](https://github.com/J00MZ/opentelemetry-rust-instrumentation), explicitly modeled on the real `open-telemetry/opentelemetry-go-instrumentation` but not itself an OTel-org project) is already attempting uprobe-based Rust auto-instrumentation via symbol demangling and multi-return-point uprobes, and is already attempting an executor-level heuristic for async span reconstruction — i.e. attempting H2 without compiler help.

**Hypotheses requiring experimental validation:**
- H1: Compiler-emitted function metadata materially improves probe selection precision *beyond what v0 demangling already provides*. **[Revised by verification]** The comparison baseline is not OBI (which does no Rust function probing at all) but projects like J00MZ, which already does symbol-based probe selection without compiler help — so H1 is really asking whether compiler metadata beats *that*, not whether it beats nothing.
- H2: Logical async spans can be reconstructed at runtime from poll-level uprobe events plus compile-time state-machine metadata.
- H3: Offset-based probe attachment via build-ID-keyed metadata works reliably on stripped, PIE, optimized release binaries.
- H4: The combined overhead of a metadata-guided eBPF approach is lower than compile-time instrumentation for equivalent span coverage.

**[Inference]** H2 is the load-bearing one. If H2 is false, "compiler-assisted eBPF for Rust" reduces to "a nicer configuration format for uprobes" — useful, but not a research contribution, and not worth restructuring the project around.

**Open questions:**
- ~~How does the J00MZ project actually resolve axum/hyper handlers from symbols, and where does it fail?~~ **Partially answered in verification** (§4.5): symbol-table scan + `rustc-demangle` + per-library pattern matching + multi-return-point uprobes + JSON/DWARF/heuristic struct-offset tracking. Failure modes (accuracy under optimization, version drift) are not published and remain open.
- Is there a stable task identity in Tokio observable from eBPF? (If not, H2 is likely false.)
- Does OBI have an extension point that would accept externally supplied instrumentation metadata, or would this require a fork?
---

---

← [Rust-Specific Instrumentation Challenges](06-rust-specific-challenges.md) · [Contents](README.md) · [Competitive / Adjacent Landscape](08-competitive-landscape.md) →
