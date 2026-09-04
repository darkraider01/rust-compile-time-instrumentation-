← [Gap Analysis](09-gap-analysis.md) · [Contents](../README.md) · [Recommended Architecture](11-recommended-architecture.md) →

---

## 10. Architecture Candidates


### Architecture A — Source transformation behind a Cargo build hook

**[Revised after the adversarial review round — see [Appendix C](appendix-c-adversarial-review.md).** An independent review challenged whether this architecture can instrument third-party dependencies on stable Rust at all. It was tested directly: it can, two different ways, with no nightly flags. The diagram and technology list below reflect the corrected design — extern `"C"` trampolines instead of injected crate dependencies, byte-range source splicing instead of `syn`→`prettyplease`, and an isolated `--target-dir` instead of a `RUSTFLAGS`-based cache-buster.]**

**[Revised again after the maintainer Q&A round — see [Appendix D.2](appendix-d-maintainer-qa.md).** The runtime layer changed: generated code now calls the **native OpenTelemetry API** (`opentelemetry::trace::FutureExt::with_context` for async), not `tracing`. `tracing`, `tracing-subscriber`, and `tracing-opentelemetry` leave the injected dependency set entirely. **This is the only architecture still live** — Architectures C and D were closed by the same round ([Appendix D.4](appendix-d-maintainer-qa.md)).]**

```
 cargo instrument -- build
        │
        ├─ PHASE 1: SETUP (out-of-band, before Cargo starts)
        │     cargo metadata ──► resolve dependency graph
        │     match graph against rule set (*.rules.yml)
        │     pre-build the runtime crate standalone ◄─ defeats topological
        │                                                 scheduling; it is
        │                                                 never a Cargo DAG node
        │     emit .instrument-build/plan.json
        │
        └─ PHASE 2: BUILD
              cargo build --target-dir target/instrumented   ◄─ cache isolation;
                RUSTC_WRAPPER=instrument-wrapper                 NOT RUSTFLAGS
                    │
                    ▼
              for each rustc invocation (app crates AND dependencies):
                  ├─ read --crate-name, --edition, crate root path
                  ├─ is this crate in the plan?  ──no──► exec real rustc unchanged
                  └─ yes:
                        syn::parse_file  ──►  ANALYSIS ONLY (find targets, check exclusions)
                        span().byte_range()  ──►  splice into the ORIGINAL UTF-8 buffer
                        inject:  unsafe extern "C" {
                                     fn __otel_span_enter(name: *const u8, len: usize);
                                     fn __otel_span_exit(..);
                                 }
                                 (no --extern, no -L, no crate-graph involvement)
                        mirror the crate's source tree (preserves mod/include!/#[path])
                        exec real rustc on the mirrored, spliced root
                    │
                    ▼
              stock stable rustc ──► stock LLVM ──► binary
                    │                    runtime crate is a normal dependency of the
                    │                    APPLICATION; __otel_span_* resolves at final link
                    ▼
              runtime: opentelemetry::trace (NATIVE API — span kind, links)
                       async sites wrapped via FutureExt::with_context
                          ─► opentelemetry_sdk ─► opentelemetry-otlp ─► OTLP
```

| | |
| --- | --- |
| **How it works** | Two phases exactly mirroring `otelc`. Phase 1 resolves the dependency graph and pre-builds the runtime crate standalone, outside Cargo's own DAG — this is what defeats the "topological scheduling" objection, since Cargo never has to know the runtime exists as a graph node. Phase 2 intercepts each `rustc` invocation (including third-party dependencies, not just workspace members), splices instrumentation into the original source buffer by byte offset, and delegates to the real compiler. |
| **Required technologies** | `cargo metadata`, `RUSTC_WRAPPER`, `syn` (parsing/analysis only — no `prettyplease`), `serde`/`serde_yaml` for rules, `opentelemetry` (native traces API, incl. `FutureExt`), `opentelemetry_sdk`, `opentelemetry-otlp`. **No `tracing`, `tracing-subscriber`, or `tracing-opentelemetry`** ([Appendix D.2](appendix-d-maintainer-qa.md)) |
| **Advantages** | Stable Rust — **confirmed by direct experiment, not merely believed**, including for crates that do not declare the runtime as a dependency (Appendix C.2). Reaches dependencies. Byte-splice output preserves comments, formatting, and line numbers exactly outside the insertion point (Appendix C.6), which is the single biggest debugging advantage of this architecture. Proven design (`otelc`). Incremental: rules can be added one at a time. Low risk of miscompilation, because a stock compiler validates everything generated. The extern `"C"` trampoline needs no `--extern`/`-L` propagation and so cannot produce duplicate-crate or dependency-cycle errors. |
| **Disadvantages** | Cannot see macro-generated code. No type information (so rules match syntactically, not semantically). Cannot instrument `std`. Rewriting dependency source requires care with `include!`, `#[path]`, `build.rs`-generated modules, and `cfg` — mirroring the source tree (not just the entry file) is required and untested at scale (§15.5 Q1 in Appendix C). |
| **Risks** | Cargo's rebuild fingerprint does not track `RUSTC_WRAPPER` at all (Appendix B item 2) — mitigated with an isolated `--target-dir`, not `RUSTFLAGS`. Rules matching syntactically will have false positives on shadowed names. LTO + `codegen-units=1` + `panic=abort` interaction with the trampoline was verified on Windows/MSVC (E-7) and Linux/ELF (E-9), closing open question Q7/FE-1 (macOS remains open). |
| **Runtime overhead** | Same as hand-written native-API instrumentation — one span per instrumented call. **[Revised, Appendix D.2]** There is no `STATIC_MAX_LEVEL` analogue in the native API, so the kill switch is the dedicated `--cfg` gate on generated code (which was required anyway — `STATIC_MAX_LEVEL` is global and additive, Appendix C.1). With the gate on but the SDK not recording, the residual cost is a non-recording span rather than nothing — see [R23](13-technical-risks.md). |
| **Build overhead** | **[Hypothesis, now anchored to real data — [Appendix D.3](appendix-d-maintainer-qa.md)]** `otelc`'s measured CodSpeed figures are 5.3 s → 19.9 s (+275%) single-package and 17.4 s → 26.8 s (+54%) multi-package, i.e. a large fixed setup cost that amortises over bigger builds. **Expect roughly 1.5×–3× clean compile time**, worst on small projects. Byte-splicing avoids the re-parse-and-print cost of the original design, so we may land better; still unmeasured for Rust ([§14.3](14-evaluation-plan.md)). |
| **Rust-version compatibility** | Any stable Rust that `syn` can parse. Effectively "recent stable and older." |
| **Development complexity** | Medium. The rule engine, source-tree mirroring, and Cargo/`rustc` argument plumbing are the bulk of it, not the splicing logic itself. |

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
        runtime crate ─► native OTel (or tracing bridge) ─► OTLP
```

| | |
| --- | --- |
| **How it works** | Replace `rustc` with a driver that overrides a MIR query, mutating the MIR body to call into a runtime crate at entry and exit. |
| **Required technologies** | Nightly + `rustc-dev` + `llvm-tools` components, `rustc_driver`/`rustc_interface`/`rustc_middle`, `rustc_plugin` for Cargo integration, a `#[no_mangle] extern "C"` runtime crate with `#[used]` anchors |
| **Advantages** | Sees everything, including macro-generated and derive-generated code. Type-aware, so rules can match semantically ("all methods of any type implementing `Repository`"). Pre-monomorphization, so one edit per generic definition. Can see drop points and unwind edges, enabling correct panic handling. |
| **Disadvantages** | Nightly-pinned to a specific date, forever. **[Fact]** Unshippable to teams with an MSRV policy. Runtime linkage is fragile under LTO. Injected MIR must be valid or the compiler ICEs. Debugging is by ICE and `-Z dump-mir`. Async is *harder* here, not easier (§6.3). |
| **Risks** | Highest of all architectures. Every nightly bump is a potential rewrite. A subtle MIR bug produces a miscompiled user program, which is far worse than a missing span. |
| **Runtime overhead** | Comparable to A if the runtime is the same; potentially lower if we can inject cheaper primitives than full tracing/OTel spans |
| **Build overhead** | **[Hypothesis]** Lower than A (no re-parse, no re-print), but every MIR body is cloned. Must be measured. |
| **Rust-version compatibility** | Exactly one nightly per release. |
| **Development complexity** | High, and the complexity is *ongoing*. |

### Architecture C — Compiler-generated metadata → binary → eBPF → OTel — ⛔ **ABANDONED**

> **[Closed 2026-09-04 — see [Appendix D.4](appendix-d-maintainer-qa.md).]** OBI maintainer Giuseppe Ognibene has a working prototype of Tokio async task reconstruction and context propagation in eBPF (OBI issue #1096), which resolves H2 — the hypothesis this architecture existed to test. The [§15.6](15-final-recommendation.md) pivot condition is formally triggered. **We will not build an eBPF loader or a competing implementation**; we review and contribute to #1096 instead. Retained below as a research record.

**[Revised after the adversarial review round — see [Appendix C.4](appendix-c-adversarial-review.md).** The bespoke JSON sidecar keyed by build ID, shown in the original diagram, was a design error: Userland Statically Defined Tracing (USDT) has solved "compile-time probe metadata embedded in the binary for eBPF consumption" for two decades, and mature, stable-Rust crates (`oxidecomputer/usdt`, `cuviper/probe`) already emit it. The sidecar is replaced with ELF-native USDT notes below. What is **not** solved by USDT, and remains the actual open contribution, is encoding Rust's async state-machine structure — the diagram now reflects that narrower target.]**

```
 build (stock rustc + a USDT-emitting crate, e.g. oxidecomputer/usdt)
        │
        ▼
 binary  (+ .note.gnu.build-id, + .note.stapstd USDT probes)
        USDT already carries: probe name, argument count/types/locations
        │
        ▼
 [THE OPEN PART] does a probe additionally carry async structure?
        { probe: "handle_checkout::poll", state_variant: 3,
          await_site: "src/handlers.rs:42", logical_name: "handle_checkout" }
        ── requires extracting rustc's StateTransform await↔state-variant
           map (§6.3) and encoding it alongside the standard USDT note ──
        │
        ▼
 loader (Aya / bpftrace / libbpf — any off-the-shelf USDT-aware consumer)
        no custom loader needed for the probe-attachment part;
        only the async-reconstruction logic is bespoke
        │
        ▼
 BPF programs ─► perf/ring buffer ─► userspace collector
        │
        ▼
 reconstruct spans (H2, unvalidated) ─► OTLP
```

| | |
| --- | --- |
| **How it works** | The build emits standard USDT probes via an existing crate — no bespoke sidecar, no build-ID join logic to maintain, and any off-the-shelf USDT-aware tool (`bpftrace`, `libbpf`, Aya) can already attach to the *probe-name-and-arguments* part with zero custom loader code. The only genuinely new work is whether a probe's payload can additionally carry Rust's async state-machine structure, which USDT was never designed to express. |
| **Required technologies** | `oxidecomputer/usdt` or `cuviper/probe` (stable Rust), Aya or `bpftrace` for consumption, a nightly `rustc` driver only if extracting the `StateTransform` await↔variant map (source-derived metadata does not need this), a userspace span reconstructor, OTLP export |
| **Advantages** | Instrumentation can be attached and detached at runtime with zero cost when off (a single `nop` per probe). One build serves both instrumented and uninstrumented operation. Standard tooling can already consume the non-async-specific parts — no custom loader is required just to get symbol-accurate probe attachment. No metadata/binary drift for the standard part, since USDT notes live inside the ELF itself rather than a separate file. |
| **Disadvantages** | Linux-only. Requires elevated privileges (`CAP_BPF`, `CAP_SYS_PTRACE`, and for some paths `CAP_NET_ADMIN`) **[Fact]**. uprobe cost per hit. Async span reconstruction depends on H2, which is unvalidated — and H2 is now understood to depend specifically on whether the `StateTransform` mapping can be extracted and encoded, not merely on "compiler metadata" in general. |
| **Risks** | H2 may be false, in which case this yields correct probe-level events (a real improvement over symbol guessing) but not logical async spans. Inlined functions have no probe point. Kernel-version and lockdown-mode constraints. |
| **Runtime overhead** | **[Open question]** Zero when detached (USDT's standard `nop`-sled property); per-hit trap cost when attached. Must be measured, never asserted. |
| **Build overhead** | Low for the standard USDT part (stable, source-level). Higher and nightly-gated only for the async-structure extension, if pursued. |
| **Rust-version compatibility** | The standard USDT part: stable, today, via existing crates. The async-structure extension: nightly (`rustc_private`, same constraints as Architecture B). |
| **Development complexity** | Substantially lower than the original design for the standard part, since it reuses existing crates instead of inventing a sidecar format. The async-structure extension remains high complexity, spread across compiler tooling and kernel tooling. |

### Architecture D — Compiler-assisted instrumentation: hybrid — ⛔ **ABANDONED**

> **[Closed 2026-09-04 — see [Appendix D.4](appendix-d-maintainer-qa.md).]** D was A plus C, and C is closed, so D reduces to A. Its structural argument — *"the only architecture where the eBPF half being disappointing does not sink the project"* — was sound and did its job: A was designed to stand alone, and it now does. The division of labour survives the closure in a better form than D proposed: **compile-time instrumentation covers the platforms eBPF cannot reach** (macOS, Windows, unprivileged containers, non-root), and OBI covers Linux-with-privileges, with no duplicate-span reconciliation problem between them because they are separate tools. Retained as a research record.

```
 cargo instrument -- build --mode=hybrid
        │
        ├─ Phase 1: analysis (as in A)
        ├─ Phase 2: source rewriting for HOT/SEMANTIC boundaries
        │             (HTTP handlers, DB calls, spawn sites)
        │             → real tracing spans, correct async semantics,
        │               real context propagation
        └─ Phase 3: emit USDT probes for EVERYTHING ELSE (§10, Arch. C)
                      → consumed by any USDT-aware loader (Aya/bpftrace)
                        for on-demand deep-dive probing, no sidecar file
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

← [Gap Analysis](09-gap-analysis.md) · [Contents](../README.md) · [Recommended Architecture](11-recommended-architecture.md) →
