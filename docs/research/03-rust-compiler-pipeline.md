← [OpenTelemetry Go Compile-Time Instrumentation](02-otelc-go.md) · [Contents](../../README.md) · [Existing Rust Instrumentation Landscape](04-rust-instrumentation-landscape.md) →

---

## 3. The Rust Compilation Pipeline


Primary sources:
- [rustc-dev-guide: Overview of the compiler](https://rustc-dev-guide.rust-lang.org/overview.html)
- [`rustc_mir_transform/src/lib.rs`](https://github.com/rust-lang/rust/blob/master/compiler/rustc_mir_transform/src/lib.rs)
- [The rustc book](https://doc.rust-lang.org/rustc/)
- [Cargo config reference](https://doc.rust-lang.org/cargo/reference/config.html)
- [The Unstable Book: `rustc_private`](https://doc.rust-lang.org/stable/unstable-book/language-features/rustc-private.html)
- [rustc-dev-guide: Remarks on perma-unstable features](https://rustc-dev-guide.rust-lang.org/rustc-driver/remarks-on-perma-unstable-features.html)

### 3.1 The pipeline

```
source text
    │  rustc_lexer / rustc_parse
    ▼
token stream ──── macro_rules! + proc-macro expansion (rustc_expand) ───┐
    │                                                                   │
    ▼                                                                   │
AST  ◄──────────────────────────────────────────────────────────────────┘
    │  name resolution, AST validation, early lints
    │  rustc_ast_lowering  (desugars: loops, `?`, async/await, elided lifetimes)
    ▼
HIR
    │  type checking, trait resolution (rustc_hir_analysis, rustc_hir_typeck)
    ▼
THIR   (fully typed; method calls and implicit derefs made explicit)
    │  rustc_mir_build
    ▼
MIR (Analysis phase)
    │  borrow check (rustc_borrowck)
    │  run_analysis_cleanup_passes
    │  run_runtime_lowering_passes   ← drop elaboration, coroutine::StateTransform
    │  run_runtime_cleanup_passes
    ▼
MIR (Runtime phase)  ── query: mir_drops_elaborated_and_const_checked
    │  run_optimization_passes
    ▼
MIR (Optimized)      ── query: optimized_mir
    │  monomorphization + CGU partitioning (rustc_monomorphize, rustc_codegen_ssa)
    ▼
LLVM IR
    │  LLVM optimization pipeline (incl. inlining)
    ▼
object code
    │  linker
    ▼
binary
```

**[Fact]** The stages between HIR and LLVM IR are *queried*, not run as a fixed pass pipeline; `TyCtxt` is the central context holding all queries and caches. This query architecture is what makes external query overriding possible at all.

### 3.2 Stage-by-stage instrumentation analysis

#### 3.2.1 Source text (pre-lexing)

| | |
| --- | --- |
| **Information available** | Raw text exactly as written: comments, formatting, `cfg` attributes as authored. |
| **Can instrument?** | Yes - textual/AST rewriting before `rustc` reads the file. |
| **APIs / tooling** | `syn` + `quote` + `prettyplease` (parse → transform → print); `proc-macro2` for spans; `ra_ap_syntax` (rust-analyzer's lossless CST) for edit-preserving rewrites. All **stable**, all usable outside a proc-macro context. **[Revised - [ADR-002](17-decision-records.md)]** The project uses **none of these three pipelines as written**: `syn` parses for *analysis only*, and the edit is a byte-range splice into the original UTF-8 buffer at `span().byte_range()` (`cargo-mutants`' technique). `prettyplease` is not used at all - a measured round-trip destroyed every non-doc comment and reformatted the whole file ([Appendix E](appendix-e-experiment-matrix.md) E-3) - and `ra_ap_syntax` is therefore never needed. |
| **Advantages** | Stable Rust. Output is fully inspectable, which makes debugging tractable. Diagnostics can be mapped back to real source. Zero compiler-version coupling. Works on dependency source, because Cargo has already unpacked it under `~/.cargo/registry/src`. |
| **Disadvantages** | No type information. Cannot resolve `use` aliases, method receivers, or trait impls reliably. Cannot see through macros - you see the invocation, not its output. `syn` round-tripping destroys formatting unless you use a CST-based rewriter. |
| **Stability concerns** | Minimal. `syn` is among the most-depended-on crates in the ecosystem. |
| **Maintenance burden** | Low. Tracks *syntax* evolution (new keywords, new syntax), not compiler internals, and `syn` absorbs most of that. |

#### 3.2.2 AST (post-parse, pre-expansion)

**[Fact]** Rust removed compiler plugins (`#![feature(plugin)]`); there is no supported way to register an in-process AST pass. **[Fact]** The only sanctioned AST-manipulation mechanism is procedural macros, which are (a) opt-in per item, (b) unable to see outside the token stream handed to them, and (c) impossible to apply to a crate you do not own.

| | |
| --- | --- |
| **Can instrument?** | Only via proc macros, i.e. only for code that opts in. Not a route to automatic whole-graph instrumentation. |
| **Advantages** | This is what `#[tracing::instrument]` already is; battle-tested. |
| **Disadvantages** | Not automatic; cannot reach dependencies. |
| **Verdict** | Not a viable automatic whole-graph mechanism. (Earlier considered as a potential generation target via `#[tracing::instrument]`, but superseded by native OTel API generation per ADR-001.) |

#### 3.2.3 HIR

| | |
| --- | --- |
| **Information available** | Desugared control flow, resolved names, item structure. **async/await has already been desugared by this point** (`rustc_ast_lowering`). Types not yet computed. |
| **Can instrument?** | In principle via query overriding, but there is no "rewrite HIR" query that codegen depends on the way it depends on `optimized_mir`. HIR is treated as an input artifact, not a rewritable one. |
| **Advantages** | Names resolved - you know what `foo::bar` actually refers to. |
| **Disadvantages** | No practical injection point. `rustc_private` only. |
| **Verdict** | **Not a viable injection layer.** Valuable for *analysis* (deciding what to instrument), not for *injection*. |

#### 3.2.4 THIR

| | |
| --- | --- |
| **Information available** | Fully typed; method calls and implicit derefs made explicit. **[Fact]** |
| **Can instrument?** | Theoretically (a `thir_body` query exists), but THIR is a short-lived intermediate consumed immediately by `rustc_mir_build`, and is the least documented and least externally used IR. |
| **Verdict** | **Not recommended.** Higher fragility than MIR with no compensating advantage. |

#### 3.2.5 MIR - the serious compiler-level option

**[Fact]** MIR is a control-flow graph of basic blocks containing `Statement`s, each terminated by a `Terminator` (including `Call`). It is *pre-monomorphization* and generic: a generic function has one MIR body regardless of how many times it is instantiated.

**[Fact - read from rustc source]** The MIR pipeline is:

```
mir_built                                   (Analysis::Initial)
  → borrowck
  → mir_drops_elaborated_and_const_checked
        run_analysis_cleanup_passes         → Analysis::PostCleanup
        run_runtime_lowering_passes         → Runtime::Initial
              CriticalCallEdges, PostAnalysisNormalize, Subtyper,
              ElaborateDrops, CheckDropRecursion, AbortUnwindingCalls,
              AddMovesForPackedDrops, EraseDerefTemps,
              ElaborateBoxDerefs, coroutine::StateTransform, KnownPanicsLint
        run_runtime_cleanup_passes          → Runtime::PostCleanup
  → optimized_mir
        run_optimization_passes             → Runtime::Optimized
```

**[Fact - critical for §6]** `coroutine::StateTransform` runs inside `run_runtime_lowering_passes`, which runs inside `mir_drops_elaborated_and_const_checked`, which `optimized_mir` calls before running optimization passes. **MIR obtained from the `optimized_mir` query has therefore already been through the async state-machine transform.**

**[Fact]** `rustc_mir_transform::run_analysis_to_runtime_passes` carries this source comment:

> `// Made public so that mir_drops_elaborated_and_const_checked can be overridden`
> `// by custom rustc drivers, running all the steps by themselves. See #114628.`

The compiler explicitly accommodates custom drivers overriding MIR queries. That is a meaningful signal: this is a supported-in-practice (if unstable-in-contract) extension point.

**Instrumentation mechanics.** From [Emanuele Vannacci, "Rust MIR Instrumentation" (2025)](https://emavan.com/blog/2025/mir-instrumentation/), corroborated against rustc source:

```rust
#![feature(rustc_private)]
extern crate rustc_driver;
extern crate rustc_interface;
extern crate rustc_middle;

const CUSTOM_OPT_MIR: for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> &'tcx Body<'tcx> =
    |tcx, def| {
        let mut body = (rustc_interface::DEFAULT_QUERY_PROVIDERS.optimized_mir)(tcx, def).clone();
        MyInstrumentationPass.run_pass(tcx, &mut body);
        tcx.arena.alloc(body)
    };

impl rustc_driver::Callbacks for MyCallbacks {
    fn config(&mut self, config: &mut rustc_interface::Config) {
        config.override_queries = Some(|_sess, queries| {
            queries.optimized_mir = CUSTOM_OPT_MIR;
        });
    }
}
```

The pattern is: call the default provider, clone the `Body` (you need an owned one to edit), mutate it, arena-allocate the result.

**Linking the runtime.** **[Fact]** Injected MIR calls must resolve to a real crate. The documented approach is for the driver to append flags to its own `rustc` invocation:

```
-Zunstable-options  -L<runtime_dir>  --extern=force:runtime=<runtime_dir>/libruntime.rlib
```

`--extern=force:` makes the crate available even though no source line references it. **[Fact]** Under LTO the linker may still strip the runtime, because it is only referenced from injected MIR; the documented mitigation is a `#[no_mangle] extern "C"` hook anchored with a `#[used] static`. **[Fact]** The same source warns against passing typed references (`&T`) across the hook boundary - it is brittle and can produce invalid IR under LTO - and recommends passing a `usize` address obtained via `CastKind::PointerExposeProvenance`.

| | |
| --- | --- |
| **Information available** | Full types, resolved calls, CFG, drop points, unwind edges, source spans. Pre-monomorphization, so one edit per generic definition. |
| **Can instrument?** | **Yes.** This is the real compiler-level option. |
| **APIs** | `rustc_driver::run_compiler` + `rustc_interface::Config::override_queries`. Requires `#![feature(rustc_private)]` and the `rustc-dev` and `llvm-tools` rustup components. |
| **Advantages** | Sees everything the compiler compiles, including dependencies, macro output, and derive-generated code. Type-aware. Instrumentation cost is per-definition, not per-instantiation. |
| **Disadvantages** | Nightly-only, permanently. Runtime-crate plumbing is fiddly and LTO-sensitive. Hand-built MIR is easy to get wrong; invalid MIR ICEs the compiler or miscompiles silently. |
| **Stability concerns** | The worst in this document. **[Fact]** "By its very nature, the internal compiler APIs are always going to be unstable." |
| **Maintenance burden** | High and *continuous*, not one-time. See §3.5. |

#### 3.2.6 Monomorphization / codegen

**[Fact]** Monomorphization happens in `rustc_monomorphize`/`rustc_codegen_ssa` as MIR is lowered to LLVM IR. This is where `fn foo<T>` becomes `foo::<u32>` and `foo::<String>`.

| | |
| --- | --- |
| **Can instrument?** | Only from inside the compiler (same nightly constraints as MIR), or by writing a custom codegen backend (far more work). |
| **Notable** | This is the only layer that can make *per-instantiation* decisions - "instrument `Repository::<PgPool>::find` but not `Repository::<MockPool>::find`." No layer above can express that; no layer below retains the type names to express it against. |

#### 3.2.7 LLVM IR

**[Fact]** rustc already ships instrumentation flags operating at this level:

| Flag | Stability | What it does |
| --- | --- | --- |
| `-C instrument-coverage` | **Stable** ([rustc book](https://doc.rust-lang.org/rustc/instrument-coverage.html), stabilized in [PR #90132](https://github.com/rust-lang/rust/pull/90132)) | LLVM source-based coverage counters |
| `-Z instrument-xray` | Nightly ([PR #102963](https://github.com/rust-lang/rust/pull/102963)) | XRay function entry/exit sleds, with `always` / `never` / `ignore-loops` / `instruction-threshold` / `skip-entry` / `skip-exit` |
| `-Z instrument-mcount` | Nightly | `mcount()` call in each function prologue; has had breakage ([#92109](https://github.com/rust-lang/rust/issues/92109)) |
| `-Z llvm-plugins` | Nightly ([PR #91125](https://github.com/rust-lang/rust/pull/91125)) | Load out-of-tree LLVM pass plugins |

**[Inference - important]** `-Z instrument-xray` is *already* a compiler-inserted function entry/exit hook mechanism with configurable filtering. If our goal were purely "get a callback on every function entry and exit," a large part of it exists in the compiler and we would be reimplementing it badly. Our value has to come from *semantics* (which function, why, with what attributes, in what async context), not from the mechanical act of hooking entry/exit.

| | |
| --- | --- |
| **Information available** | Monomorphized, mangled symbols; debug info if enabled. Rust-level semantics (traits, generics, async structure) largely erased. |
| **Advantages** | Language-agnostic tooling (`llvm-plugin-rs`, XRay). Post-inlining decisions possible. |
| **Disadvantages** | You have lost the information that makes a span *meaningful*. You know "symbol `_RNvCs…` was entered," not "this is an HTTP handler for route X." Inlining has already destroyed many function boundaries. |
| **Verdict** | Good for profiling. Poor for semantic tracing. |

#### 3.2.8 Binary / machine code

| | |
| --- | --- |
| **Information available** | Symbol table (unless stripped), DWARF (if built with debuginfo), GNU build ID, ELF sections. |
| **Can instrument?** | Externally, via uprobes (§7) or static binary rewriting. |
| **Advantages** | No rebuild required. Language-agnostic. Can be attached and detached at runtime. |
| **Disadvantages** | Everything above is gone. Async structure is unrecognisable. Inlined functions do not exist as symbols. Generic instantiations are separate opaque symbols. |

### 3.3 Cargo integration mechanisms - the `-toolexec` analogues

**[Fact]** From the Cargo config reference:

| Mechanism | Env var | Scope | Invocation |
| --- | --- | --- | --- |
| `build.rustc` | `RUSTC` | Everything | Replaces the `rustc` binary outright |
| `build.rustc-wrapper` | `RUSTC_WRAPPER` | **Everything, including dependencies** | `$RUSTC_WRAPPER /path/to/rustc [args]` |
| `build.rustc-workspace-wrapper` | `RUSTC_WORKSPACE_WRAPPER` | Workspace members only | Nests: `$RUSTC_WRAPPER $RUSTC_WORKSPACE_WRAPPER $RUSTC [args]` |
| `build.rustflags` | `RUSTFLAGS` / `CARGO_ENCODED_RUSTFLAGS` | Target platform, all crates | Extra flags appended |

**[Fact]** The wrapper receives the path to the real `rustc` as its first argument, followed by the full compiler argument list - including every `--extern`, `-L`, `--cfg`, `--crate-name`, and the path to the crate root source file. That is everything needed to (a) know which crate is being compiled, (b) find its source, and (c) re-invoke the real compiler on rewritten source.

~~**[Fact]** `RUSTC_WORKSPACE_WRAPPER` affects the artifact filename hash, so wrapped and unwrapped builds cache separately. **[Open question]** Whether `RUSTC_WRAPPER` participates in the fingerprint identically must be verified experimentally.~~

**[RESOLVED, and the original claim was wrong - [Fact], confirmed by direct experiment: [Appendix E](appendix-e-experiment-matrix.md) E-1.]** **Neither** wrapper participates in Cargo's artifact rebuild fingerprint. Building a crate, then setting `RUSTC_WRAPPER` with no source change, produces zero recompilation - Cargo prints `Finished`, the wrapper is never invoked for the actual compile, and the fingerprint hash is byte-identical. `RUSTC_WORKSPACE_WRAPPER` behaves the same way, so the sentence above ("wrapped and unwrapped builds cache separately") does not hold for it either. This is the worst possible failure mode for an observability tool - **it looks like it worked** - and it is the default steady state of a plain `cargo build`, not an edge case. It is [R1](13-technical-risks.md), and the mitigation is an isolated `--target-dir` ([ADR-004](17-decision-records.md)), not `RUSTFLAGS`.

**[Inference - key structural finding of §3]** `RUSTC_WRAPPER` is a near-exact analogue of Go's `-toolexec`:

| | Go | Rust |
| --- | --- | --- |
| Hook | `-toolexec` | `RUSTC_WRAPPER` |
| Granularity | package | crate |
| Sees third-party dependencies? | yes | yes |
| Sees the standard library? | yes | **no** - `std`/`core` ship precompiled as rlibs |
| Stability | stable | stable |
| Existing users | `otelc` | Clippy (`RUSTC_WORKSPACE_WRAPPER`), `sccache` (`RUSTC_WRAPPER`) |

**[Fact]** Rust's standard library ships precompiled. Instrumenting `std` requires `-Z build-std` (nightly) or a hand-built custom sysroot. **[Inference]** This is a real capability gap versus `otelc`, but a mild one for our purposes: the interesting boundaries for OTel (`hyper`, `tokio`, `sqlx`, `reqwest`, `tonic`) are all third-party crates, not `std`.

### 3.4 Stable vs. nightly: the hard line

| Mechanism | Stable? | Notes |
| --- | --- | --- |
| Proc macros (`syn`, `quote`) | ✅ | The `#[instrument]` route |
| Source/AST rewriting outside the compiler | ✅ | `syn` and `ra_ap_syntax` both work standalone |
| `RUSTC_WRAPPER` / `RUSTC_WORKSPACE_WRAPPER` | ✅ | Documented Cargo config |
| Cargo custom subcommands (`cargo-foo`) | ✅ | |
| `-C instrument-coverage` | ✅ | LLVM coverage instrumentation |
| `--emit=metadata`, `--print` queries | ✅ | Useful in the analysis phase |
| `-C symbol-mangling-version=v0` | ✅ and **default** since 1.97 | See §7.3 |
| `cargo metadata` (JSON dependency graph) | ✅ | The Rust equivalent of `go build -n` for Phase 1 analysis |
| `rustc_private` / `rustc_driver` / `rustc_interface` | ❌ nightly, permanently unstable | Requires `rustc-dev` + `llvm-tools` rustup components |
| MIR query overriding | ❌ nightly | The `optimized_mir` override route |
| Compiler plugins (`#![feature(plugin)]`) | ❌ **removed** | Does not exist any more |
| `-Z instrument-xray`, `-Z instrument-mcount` | ❌ nightly | |
| `-Z llvm-plugins` | ❌ nightly | |
| `-Z build-std` | ❌ nightly | Needed to instrument `std` |

### 3.5 The maintenance reality of nightly compiler tooling

**[Fact]** The [`rustc_plugin`](https://github.com/cognitive-engineering-lab/rustc_plugin) framework - which powers Flowistry, Aquascope, Paralegal, and Argus - states its position plainly: because the compiler interface is not stable, "the only sensible way to develop a Rust compiler plugin is by pinning to a specific nightly." Each release encodes the nightly in its version string (e.g. `0.15.2-nightly-2026-05-01`), users must match it exactly, and a nightly bump is treated as a **breaking semver change**.

**[Fact]** `rustc_plugin` documents a *maximum* supported Rust version rather than a minimum: a plugin cannot analyse code that uses compiler features newer than its pinned toolchain.

**[Inference - decisive]** For an *instrumentation* tool this constraint is far more damaging than for an analysis tool. Flowistry's users will install a specific nightly to get an IDE feature they use interactively. A production service team will not pin their entire build to `nightly-2026-05-01` in order to get traces, because that pins their `std`, their compiler bug surface, and their ability to upgrade dependencies that raise their MSRV. **A nightly-only instrumentation tool is, for practical purposes, unshippable.** This is the strongest single argument against Architecture B as a Phase 1 target.

### 3.6 Precedent: who operates at which layer

| Tool | Layer | Mechanism | Toolchain |
| --- | --- | --- | --- |
| Clippy | HIR/MIR lints | `RUSTC_WORKSPACE_WRAPPER` + custom driver | Ships with rustup, released in lockstep with rustc |
| Miri | MIR interpreter | Custom driver, `rustc_private` | Nightly (shipped with rustup) |
| Kani, MIRAI, Prusti | MIR analysis | Custom driver, `rustc_private` | Pinned nightly |
| Flowistry / Aquascope / Paralegal | MIR + HIR analysis | `rustc_plugin` | Pinned nightly |
| `sccache` | build caching | `RUSTC_WRAPPER` | Stable |
| `cargo-llvm-cov` | coverage | `-C instrument-coverage` + `llvm-tools` | Stable |

**[Inference]** Every "pinned nightly" entry is a *developer tool*, run on a developer machine or in a research setting. The only stable-Rust entries are the ones that wrap the build rather than reach into the compiler. That is the company our tool needs to keep if it is meant to be used.

---

---

← [OpenTelemetry Go Compile-Time Instrumentation](02-otelc-go.md) · [Contents](../../README.md) · [Existing Rust Instrumentation Landscape](04-rust-instrumentation-landscape.md) →
