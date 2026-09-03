← [Gap Analysis](09-gap-analysis.md) · [Contents](README.md) · [Recommended Architecture](11-recommended-architecture.md) →

---

## 10. Architecture Candidates


### Architecture A — Source/AST transformation behind a Cargo build hook

```
 cargo instrument -- run
        │
        ├─ Phase 1: ANALYSIS
        │     cargo metadata ──► dependency graph
        │     match graph against rule set (*.rules.yml)
        │     resolve which crates need rewriting
        │     ensure hook/runtime crate is a dependency (edit Cargo.toml)
        │     emit .instrument-build/plan.json
        │
        └─ Phase 2: BUILD
              cargo build  with  RUSTC_WRAPPER=instrument-wrapper
                    │
                    ▼
              for each rustc invocation:
                  ├─ read --crate-name, --edition, crate root path
                  ├─ is this crate in the plan?  ──no──► exec real rustc unchanged
                  └─ yes:
                        parse sources (syn / ra_ap_syntax)
                        apply matching rules  ──►  inject #[tracing::instrument(...)]
                        write rewritten tree to OUT_DIR/instrumented/<crate>/
                        exec real rustc on rewritten root
                    │
                    ▼
              stock rustc ──► stock LLVM ──► binary
                    │
                    ▼
              runtime: tracing ─► tracing-opentelemetry ─► opentelemetry_sdk ─► OTLP
```

| | |
| --- | --- |
| **How it works** | Two phases exactly mirroring `otelc`. Phase 1 resolves the dependency graph and prepares dependencies; Phase 2 intercepts each `rustc` invocation, rewrites source for planned crates, and delegates to the real compiler. |
| **Required technologies** | `cargo metadata`, `RUSTC_WRAPPER`, `syn` + `quote` + `prettyplease` (or `ra_ap_syntax` for edit-preserving rewrites), `serde`/`serde_yaml` for rules, `tracing`, `tracing-opentelemetry`, `opentelemetry-otlp` |
| **Advantages** | Stable Rust. Reaches dependencies. Output is inspectable Rust — the single biggest debugging advantage of this architecture. Proven design (`otelc`). Incremental: rules can be added one at a time. Low risk of miscompilation, because a stock compiler validates everything we generate. |
| **Disadvantages** | Cannot see macro-generated code. No type information (so rules match syntactically, not semantically). Cannot instrument `std`. Rewriting dependency source requires care with `include!`, `#[path]`, `build.rs`-generated modules, and `cfg`. Modified dependency sources must be written somewhere writable — the registry cache is read-only. |
| **Risks** | Cargo fingerprinting could serve a cached uninstrumented artifact (must verify). Rewriting a crate whose `syn`-parse-and-print round-trip is not exact could break it. Rules matching syntactically will have false positives on shadowed names. |
| **Runtime overhead** | Same as hand-written `#[instrument]` — one span per instrumented call, statically removable via `STATIC_MAX_LEVEL` |
| **Build overhead** | **[Hypothesis]** Moderate. Parse + print of every instrumented crate's source, plus loss of some incrementality because rewritten sources change. Must be measured. |
| **Rust-version compatibility** | Any stable Rust that `syn` can parse. Effectively "recent stable and older." |
| **Development complexity** | Medium. The rule engine and Cargo/`rustc` argument plumbing are the bulk of it, not the AST work. |

### Architecture B — rustc/MIR instrumentation via a custom driver

```
 cargo instrument-mir -- run
        │
        └─ RUSTC=instrument-driver  (or RUSTC_WRAPPER)
              │
              ▼
        custom rustc driver (nightly, rustc_private)
              rustc_driver::run_compiler
                 Callbacks::config → override_queries
                     optimized_mir  (or mir_drops_elaborated_and_const_checked)
                         │
                         ├─ clone Body
                         ├─ decide: instrument this body?
                         ├─ insert Call terminator to runtime::__span_enter at entry
                         ├─ insert Call to runtime::__span_exit before each Return
                         └─ arena-allocate
                 + inject  -L <runtime>  --extern=force:runtime=...
              │
              ▼
        stock codegen ──► LLVM ──► binary  (runtime crate linked, #[used]-anchored)
              │
              ▼
        runtime crate ─► tracing / OTel ─► OTLP
```

| | |
| --- | --- |
| **How it works** | Replace `rustc` with a driver that overrides a MIR query, mutating the MIR body to call into a runtime crate at entry and exit. |
| **Required technologies** | Nightly + `rustc-dev` + `llvm-tools` components, `rustc_driver`/`rustc_interface`/`rustc_middle`, `rustc_plugin` for Cargo integration, a `#[no_mangle] extern "C"` runtime crate with `#[used]` anchors |
| **Advantages** | Sees everything, including macro-generated and derive-generated code. Type-aware, so rules can match semantically ("all methods of any type implementing `Repository`"). Pre-monomorphization, so one edit per generic definition. Can see drop points and unwind edges, enabling correct panic handling. |
| **Disadvantages** | Nightly-pinned to a specific date, forever. **[Fact]** Unshippable to teams with an MSRV policy. Runtime linkage is fragile under LTO. Injected MIR must be valid or the compiler ICEs. Debugging is by ICE and `-Z dump-mir`. Async is *harder* here, not easier (§6.3). |
| **Risks** | Highest of all architectures. Every nightly bump is a potential rewrite. A subtle MIR bug produces a miscompiled user program, which is far worse than a missing span. |
| **Runtime overhead** | Comparable to A if the runtime is the same; potentially lower if we can inject cheaper primitives than a full `tracing` span |
| **Build overhead** | **[Hypothesis]** Lower than A (no re-parse, no re-print), but every MIR body is cloned. Must be measured. |
| **Rust-version compatibility** | Exactly one nightly per release. |
| **Development complexity** | High, and the complexity is *ongoing*. |

### Architecture C — Compiler-generated metadata → binary → eBPF → OTel

```
 build (stock or lightly modified)
        │
        ├─► binary  (+ .note.gnu.build-id)
        └─► instrument-metadata.json / custom ELF section
                { build_id, functions: [
                    { symbol, mangled, kind: async_poll,
                      source: "src/handlers.rs:42",
                      logical_name: "handle_checkout",
                      generic_of: "…", await_points: [...] } ] }
        │
        ▼
 loader (Aya, userspace)
        read metadata, join on build ID
        resolve symbols/offsets → attach uprobes/uretprobes
        pass span-site id via bpf_get_attach_cookie()
        │
        ▼
 BPF programs ─► perf/ring buffer ─► userspace collector
        │
        ▼
 reconstruct spans ─► OTLP
```

| | |
| --- | --- |
| **How it works** | The build emits a metadata sidecar describing instrumentable points; an eBPF loader consumes it to attach precise, semantically-labelled probes without rebuilding for instrumentation. |
| **Required technologies** | Metadata emission (source analysis, or a compiler driver for the richer fields), Aya, uprobes, ELF/build-ID handling, a userspace span reconstructor, OTLP export |
| **Advantages** | Instrumentation can be attached and detached at runtime with zero cost when off. One build serves both instrumented and uninstrumented operation. Metadata is a durable artifact useful beyond eBPF (§11). |
| **Disadvantages** | Linux-only. Requires elevated privileges (`CAP_BPF`, `CAP_SYS_PTRACE`, and for some paths `CAP_NET_ADMIN`) **[Fact]**. uprobe cost per hit. Async span reconstruction depends on H2, which is unvalidated. Metadata/binary drift is a new failure mode. |
| **Risks** | H2 may be false, in which case this yields poll-level events rather than spans. Inlined functions have no probe point. Kernel-version and lockdown-mode constraints. |
| **Runtime overhead** | **[Open question]** Zero when detached; per-hit trap cost when attached. Must be measured, never asserted. |
| **Build overhead** | Low — metadata emission only. |
| **Rust-version compatibility** | Depends on how metadata is produced. Source-derived: stable. Compiler-derived: nightly. |
| **Development complexity** | High, and spread across two very different domains (compiler tooling and kernel tooling). |

### Architecture D — Compiler-assisted instrumentation: hybrid

```
 cargo instrument -- build --mode=hybrid
        │
        ├─ Phase 1: analysis (as in A)
        ├─ Phase 2: source rewriting for HOT/SEMANTIC boundaries
        │             (HTTP handlers, DB calls, spawn sites)
        │             → real tracing spans, correct async semantics,
        │               real context propagation
        └─ Phase 3: emit metadata for EVERYTHING ELSE
                      → sidecar consumed by an optional eBPF loader
                        for on-demand deep-dive probing
        │
        ▼
 binary with baked-in spans at semantic boundaries
   + metadata enabling ad-hoc uprobe attachment elsewhere
        │
        ▼
 always-on OTLP spans  ⊕  on-demand eBPF detail
```

| | |
| --- | --- |
| **How it works** | Compile-time instrumentation handles the things it does well — semantic boundaries, async correctness, cross-process context propagation. eBPF handles the things it does well — ad-hoc, detachable, deep probing of code you did not pre-instrument. Metadata is the shared substrate. |
| **Required technologies** | Everything in A plus everything in C. |
| **Advantages** | Each mechanism does what it is actually good at. Degrades gracefully: without eBPF you still have a working tool. This is the only architecture where the eBPF half being disappointing does not sink the project. |
| **Disadvantages** | Largest surface area. Two telemetry paths must produce consistent, non-duplicated spans — a hard correctness problem in its own right. |
| **Risks** | Duplicate spans where both mechanisms cover the same function. Complexity outrunning a solo maintainer. |
| **Runtime overhead** | Sum of A (always on) and C (when attached). |
| **Build overhead** | A's plus metadata emission. |
| **Rust-version compatibility** | Stable for the A half; the metadata half is stable if source-derived. |
| **Development complexity** | Highest — but it is A plus increments, not a different thing. |

### Architecture E — LLVM-level instrumentation (documented for completeness, not recommended)

```
 cargo build  -Z llvm-plugins=libspan_pass.so   (nightly)
        │
        ▼
 LLVM new-pass-manager pass inserts calls to __span_enter/__span_exit
        │
        ▼
 binary → runtime → OTLP
```

Advantages: post-inlining decisions; language-agnostic. Disadvantages: nightly; no semantic information (you know a mangled symbol, not that it is an HTTP handler); largely duplicates `-Z instrument-xray` **[Fact]**; async is completely opaque. **Not recommended** — it is dominated by B on semantics and by A on shippability.

---

---

← [Gap Analysis](09-gap-analysis.md) · [Contents](README.md) · [Recommended Architecture](11-recommended-architecture.md) →
