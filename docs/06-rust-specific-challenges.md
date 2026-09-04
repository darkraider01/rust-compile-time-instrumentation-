← [OpenTelemetry Rust](05-otel-rust.md) · [Contents](../README.md) · [eBPF as a Future Extension](07-ebpf-future.md) →

---

## 6. Rust-Specific Instrumentation Challenges


For each construct: what makes it hard, and at which layer(s) it is tractable.

### 6.1 Ordinary functions

Easy at every layer. Source: add `#[instrument]`. MIR: prepend a call at the entry block, append at every `Return` terminator. Binary: uprobe on the symbol.

**Complication:** every layer must decide what to *name* the span and which arguments to record. Recording all arguments (the `#[instrument]` default) is wrong for automatic instrumentation — it can leak secrets and it can be expensive (`Debug`-formatting a large struct on every call). **Automatic instrumentation must never capture argument values by default** (`skip_args` in our rule vocabulary, [§12.6](12-mvp-definition.md)). This is a security requirement, not a performance preference: an auto-instrumentation tool that records every argument by default will exfiltrate passwords, tokens, and PII into a telemetry backend without anyone deciding to.

**MVP:** yes.

### 6.2 Methods, trait methods, generics, monomorphization

**Methods (inherent impls).** Source-level: straightforward, but `self` must be skipped or the span records the whole receiver.

**Trait methods.** Three distinct sites: the trait's default body, each `impl` block's body, and the call site. Instrumenting the trait definition's default method covers only impls that do not override it. Instrumenting each impl gives correct per-implementation spans but multiplies span count. Dynamic dispatch through `dyn Trait` means the call site cannot know which impl runs.

**[Inference]** For source-level instrumentation, instrument `impl` blocks, not trait definitions, and include the implementing type in the span name (`<PgRepository as Repository>::find`). This is exactly what `#[instrument]` on the impl method produces if we set the span name explicitly.

**Generics and monomorphization.** **[Fact]** MIR is pre-monomorphization: one body per generic definition. Source is also pre-monomorphization. LLVM IR and the binary are post-monomorphization: `n` symbols for `n` instantiations.

Consequences:

| Layer | Span identity for `fn parse<T: Deserialize>(…)` |
| --- | --- |
| Source / MIR | One instrumentation site; span name is `parse`, and the concrete `T` is **not** known unless we add `std::any::type_name::<T>()` at runtime (which is cheap and gives a static string) |
| LLVM / binary | `n` distinct symbols, each mangled with the concrete type; the type is recoverable by demangling but you must attach `n` probes |

**[Inference]** This is a genuine, if narrow, advantage of the binary layer, and a genuine advantage the compiler has over it too: the compiler *knows* the instantiation set at monomorphization time and could emit it as metadata, whereas an external tool must discover it by demangling every symbol. Noted for §7.

**MVP:** inherent methods and free functions yes; trait impls yes; generics yes (one site per definition, optionally recording `type_name` of the generic parameters).

### 6.3 Async functions — the central problem

> **What happens to `async fn foo() { … }` during compilation, and why does the layer of instrumentation change the semantics?**

**The compilation path.** **[Fact]**

1. **AST → HIR (`rustc_ast_lowering`).** `async fn foo() -> T` is desugared into a function `fn foo() -> impl Future<Output = T>` whose body is an async block. The async block becomes a **coroutine** — a distinct closure-like entity with its own `DefId` and its own MIR body.
2. **MIR construction.** Two bodies now exist: the outer `foo`, whose entire job is to capture arguments and construct the coroutine value and return it; and the coroutine body, which contains the code the developer wrote, with `yield` points where the `.await`s were.
3. **`coroutine::StateTransform`** (in `run_runtime_lowering_passes`, i.e. inside `mir_drops_elaborated_and_const_checked`, i.e. **before** `optimized_mir` returns) **[Fact]** inverts the control flow of the coroutine body into a state machine. Each yield point becomes a state variant; locals live across a suspension point are moved into the coroutine struct. The resulting layout is:

   ```
   struct Coroutine {
       upvars…,
       state: u32,          // 0 = unresumed, 1 = returned, 2 = poisoned, 3.. = suspension points
       mir_locals…,         // only those live across suspensions
   }
   ```

   The pass produces a resume function (`Future::poll`) that switches on `state` to jump to the right resumption point, and drop glue that drops the right subset of fields for the current state.
4. **Codegen.** The state machine becomes an ordinary type with an ordinary `poll` method. The word "async" no longer exists.

**Why the layer changes the semantics — concretely.**

| Layer | What "instrumenting `foo`" naturally means | Resulting span duration |
| --- | --- | --- |
| **Source — native OTel (`FutureExt::with_context`)** ← **what we generate** | Wrap the returned future so the context is `attach()`ed at the start of each `poll` and detached on yield | **Correct.** The span brackets the whole logical operation; context attachment tracks the interleaving. **[Fact, [Appendix D.2](appendix-d-maintainer-qa.md)]** No `busy`/`idle` split — the OTel data model has no field for it |
| **Source (`#[instrument]`)** | Wrap the returned future in `Instrumented`, entering/exiting the span around each `poll` | **Also correct**, by the same design. `tracing` additionally separates busy time (in `poll`) from idle time (suspended). **[Revised, [Appendix D.2](appendix-d-maintainer-qa.md)]** This row was previously the *only* correct source-layer option in this table; that was wrong, and it was the premise behind §5.4's original emitter choice |
| **Source (naïve — RAII guard at the top of the body)** | `let _g = span.enter();` (or a bare `Context::attach` guard) as the first statement of an `async fn` | **Wrong in both APIs, and this is the classic Rust instrumentation bug.** The guard is held across `.await`, so the context stays current while the task is suspended and another task runs — corrupting thread-local state globally rather than locally. `#[instrument]` and `with_context` both exist partly to prevent it ([R5](13-technical-risks.md)) |
| **MIR on the outer `foo` body** | Instrument entry/exit of the function that *constructs* the future | **Meaningless.** Measures the nanoseconds spent building a struct. The actual work has not happened yet, and may never happen if the future is dropped unpolled |
| **MIR on the coroutine body, post-`StateTransform`** | Instrument entry/exit of the `poll` function | **One span per poll.** You get N spans for a future polled N times, none of which corresponds to the logical operation |
| **MIR on the coroutine body, pre-`StateTransform`** | Instrument the pre-transform body, letting `StateTransform` split it | **Potentially correct, and this is the interesting research question.** Instrumentation inserted before the transform would be threaded through the state machine by the transform itself. But the span guard would become a local live across suspension points, hence a field of the coroutine struct — which is *exactly* the right representation, and is essentially what `Instrumented` achieves by hand |
| **LLVM / binary (uprobe on `foo`)** | Probe the symbol `foo` | Measures future construction. Meaningless, as above |
| **LLVM / binary (uprobe on `poll`)** | Probe the poll function | One event per poll, no logical operation boundary, and the poll symbol is hard to attribute back to the source `async fn` |

**[Inference — the most important technical conclusion in this document]** Async Rust instrumentation is *easier* at the source layer than at any lower layer, because the source layer is the only one where the logical operation still exists as a single entity. Every layer below has already split it into "construct" and "poll N times."

**[Hypothesis — worth testing, do not assume]** MIR instrumentation inserted into a coroutine body *before* `StateTransform` would be correctly threaded into the state machine, producing a span guard stored in the coroutine struct and thus correct logical-duration semantics. This would require overriding `mir_drops_elaborated_and_const_checked` (which rustc explicitly supports overriding, per the `#114628` comment **[Fact]**) and running the pass at the right point in `run_analysis_to_runtime_passes` rather than overriding `optimized_mir`. If true, this is a genuinely novel and defensible technical result. If false — for example if the borrow checker or the transform rejects our injected locals — MIR-level async instrumentation is a dead end and Architecture B loses most of its appeal.

**MVP:** async functions **must** be supported (they are the majority of code in any real Rust service), via **`FutureExt::with_context` wrapping** at source level ([Appendix D.2](appendix-d-maintainer-qa.md)). MIR-level async is explicitly deferred to a research spike.

### 6.4 Futures, `.await`, Tokio tasks, spawned tasks

**`.await` points.** Instrumenting individual `.await` expressions (rather than function boundaries) would give await-level granularity. `tracing` has no idiomatic construct for this beyond `.instrument(span)` on the awaited future. **[Inference]** High cardinality, low value. Defer indefinitely.

**`tokio::spawn`.** A spawned task starts with a *fresh* context — context is thread-local in both APIs and does not follow a future onto another worker thread unless the future was explicitly wrapped before being spawned. This is the single most common cause of "my trace is broken across a spawn" in Rust.

**[Inference — updated for the native API, [Appendix D.2](appendix-d-maintainer-qa.md)]** A `wrap_call`-style rule that rewrites `tokio::spawn(fut)` into `tokio::spawn(fut.with_context(Context::current()))` would fix a real, common, well-known bug class automatically. **[Added]** The native API improves the ceiling here: a spawned task's relationship to its spawner is naturally an OTel **span link**, which is the one capability [Appendix C.1](appendix-c-adversarial-review.md) confirmed `tracing` cannot express — so Phase 2 can model spawn fan-out properly rather than forcing it into parent/child. It is a strong candidate for the *second* rule we implement after generic function instrumentation, and it is a good demonstration of why call-site rules (not just definition rules) are needed. It is also a good argument for copying `otelc`'s `wrap_call` rule type rather than only `inject_hooks`.

**[Resolved by specification, [§16.8](16-instrumentation-semantics.md)]** ~~Does `#[instrument]` on an `async fn` that internally spawns propagate to the spawned task?~~ Restated for the native API: an instrumented `async fn` that internally calls `tokio::spawn` does **not** propagate its context to the spawned task, under either API — the spawned future is a separate future and starts with a fresh context. This is now a specified Phase 1 limitation rather than an open question ([§16.15](16-instrumentation-semantics.md)), with a test in the MVP suite that asserts the break exists and is documented, so nobody discovers it as a surprise.

**MVP:** spawn propagation is **out** of the MVP but should be the first post-MVP feature.

### 6.5 Closures

Closures have no name, may be inlined trivially, and are frequently tiny (`|x| x + 1`). Instrumenting them automatically is almost always wrong: enormous span volume, no semantic value.

**[Inference]** Exclude closures by default at every layer. Provide opt-in via a directive-style rule if anyone ever asks.

**MVP:** excluded.

### 6.6 Panics and unwinding

A span guard is an RAII value; its `Drop` runs during unwind, so the span closes correctly on panic **[Inference]** — but nothing records *that* it panicked, so a panicking span looks like a successful one that happened to be short.

**[Fact]** `otelc` puts panic isolation in its trampoline specifically so that instrumentation cannot break the application. Our equivalent concerns:

- Under `panic = "abort"` (common in release profiles for size), `Drop` does not run during a panic at all, so the span never closes. **[Inference]** Acceptable — the process is dying.
- Instrumentation code itself must not panic. A `Debug` impl that panics, invoked while recording an argument, would turn a working program into a crashing one. This is the strongest argument for `skip_args` by default *(the rule keyword was renamed from `skip_all` with the move off `tracing` — [§12.6](12-mvp-definition.md))* and for never formatting user values without an explicit rule saying to. Specified as invariant **S7**/**S9** ([§16.2](16-instrumentation-semantics.md)), and extended there to error values: an `Err` return sets span status with **no** description, because a user error type's `Display` may allocate, panic, or carry credentials.
- `std::panic::catch_unwind` around hooks is *not* a good default: it is not free, and it is a no-op under `panic=abort`.

**MVP:** spans close on unwind via `Drop`; panics are not recorded as span status. Document it.

### 6.7 Macros and generated code

**`macro_rules!` and proc macros.** **[Fact]** Source-level tooling sees the invocation, not the expansion. A function defined inside a `macro_rules!` body, or generated by a proc macro (e.g. `#[tokio::main]`, `#[async_trait]`, `#[derive(...)]`), is invisible to a `syn`-based rewriter.

This is a **real and permanent capability difference** between source-level and MIR-level instrumentation:

| | Sees macro-generated functions? |
| --- | --- |
| Source rewriting | **No** |
| MIR / compiler-level | **Yes** — macros expanded long before |

**[Inference]** In practice this matters most for `#[async_trait]`, which is widespread and rewrites `async fn` in traits into `Pin<Box<dyn Future>>`-returning methods. Source-level instrumentation applied *before* `#[async_trait]` expands will still work, because attribute macros compose — `#[instrument]` is documented to work with `async-trait` **[Fact]**. Attribute ordering matters and must be tested.

**[Inference]** Not seeing macro-generated code is an *acceptable* MVP limitation, and arguably desirable: instrumenting `#[derive(Debug)]`-generated `fmt` methods would be pure noise.

**MVP:** macro-generated items are out of scope; the tool must not silently claim coverage it does not have.

### 6.8 Recursion

A recursive function instrumented at entry produces a span per recursion level. For a depth-10000 recursion that is 10000 spans, which will destroy the trace, the exporter, and possibly the process.

**[Inference]** Needed mitigations: a configurable max-depth guard, and/or excluding self-recursive functions by default, and/or a sampling rule. `tracing`'s own overhead per span is small but not zero, and the OTLP exporter's batching is not designed for this.

**MVP:** detect direct self-recursion syntactically and exclude it by default; document that mutual recursion is not detected.

### 6.9 Inline functions and `#[inline]`

At source and MIR level, `#[inline]` is irrelevant — instrumentation is inserted before inlining decisions. But inserting a function call into a small `#[inline]` function may prevent it being inlined, changing the program's performance characteristics.

At LLVM/binary level, inlined functions have no symbol and cannot be probed at all. **[Fact]** This is a well-known uprobe limitation.

**[Inference]** A size/complexity heuristic — do not instrument functions below N MIR statements or N AST nodes — is necessary at every layer. `-Z instrument-xray`'s `instruction-threshold` option exists for exactly this reason **[Fact]**, which is useful validation of the heuristic.

**MVP:** exclude `#[inline]`-annotated and trivially small functions by default; make the threshold configurable.

### 6.10 FFI

`extern "C"` functions and `unsafe extern` blocks: instrumenting a Rust function called *from* C means the span has no parent context (there is no ambient `tracing` context on a foreign thread) and the subscriber may not be initialised on that thread. Instrumenting a Rust wrapper *around* a C call is fine and useful.

**MVP:** exclude `extern` functions; instrument Rust-side wrappers normally.

### 6.11 `const fn`, and other hard exclusions

**[Fact]** A `const fn` cannot be instrumented in either API — `#[instrument]` on one is a compile error, and a spliced call to a non-`const` runtime function is equally a compile error. Also to exclude: functions in `const` contexts, `#[no_std]` crates without an allocator, `build.rs` scripts, proc-macro crates, and test harness code.

**[Fact — added during the Phase 0 completion audit; a hard exclusion the earlier passes missed.]** A crate carrying `#![forbid(unsafe_code)]` **cannot be instrumented at all** by the trampoline mechanism. Calling an `extern "C"` function is an unsafe operation, and `forbid` — unlike `deny` — cannot be lifted by an inner `#[allow]`; attempting it is error `E0453`. The attribute is common in the ecosystem, so this is a real reduction in reachable dependency coverage, which is the project's differentiator. **Phase 1 behaviour: skip the crate, compile it unmodified, and record the reason in the plan** ([§16.3](16-instrumentation-semantics.md), [R26](13-technical-risks.md)). Stripping a user's own safety lint in order to instrument them is not an acceptable alternative. **[Open question]** Rust 1.82+ permits `unsafe extern "C" { safe fn … }`, whose items are callable without an `unsafe` block; whether that also avoids tripping the `unsafe_code` lint is untested and cheap to test — [Appendix E](appendix-e-experiment-matrix.md) FE-3.

**[Inference]** The exclusion list is a first-class part of the design, not an afterthought. An auto-instrumentation tool that breaks the build on 3% of crates is useless, because "the build broke" is a much worse outcome than "no traces." The default posture must be: **when in doubt, do not instrument.**

### 6.12 MVP vs. deferred — summary

| Construct | MVP | Rationale |
| --- | --- | --- |
| Free functions | ✅ | Core case |
| Inherent methods | ✅ | Core case |
| Trait impl methods | ✅ | Common in real code; same mechanism |
| Generic functions | ✅ | One site per definition |
| `async fn` | ✅ **required** | Most Rust services are async; excluding it makes the tool a toy |
| `async` blocks | ⬜ deferred | Anonymous; harder to name meaningfully |
| Closures | ❌ excluded | Noise |
| `const fn` | ❌ excluded | Compile error |
| `extern "C"` fns | ❌ excluded | No ambient context |
| Recursive functions | ❌ excluded by default | Span explosion |
| `#[inline]` / trivially small | ❌ excluded by default | Perturbs optimisation; low value |
| Macro-generated items | ❌ out of scope | Invisible to source rewriting |
| Crates with `#![forbid(unsafe_code)]` | ❌ excluded (whole crate) | `forbid` cannot be lifted by `allow`; a spliced trampoline call is `E0453`. Skip and report (§6.11) |
| `async fn` **inside a dependency** | ⬜ Phase 2 | Needs the Tier-2 `core`-only future wrapper, which is designed but unproven ([§16.3](16-instrumentation-semantics.md), FE-2) |
| `tokio::spawn` context propagation | ⬜ first post-MVP feature | High value, needs call-site rules |
| Cross-process context propagation | ⬜ deferred | Needs library-specific rules |
| Argument recording | ❌ off by default | Security: PII/secret exfiltration risk |
| Panic → span status | ⬜ deferred | Needs unwind-aware hook |
| `std` instrumentation | ❌ out of scope | Needs `-Z build-std` |

---

---

← [OpenTelemetry Rust](05-otel-rust.md) · [Contents](../README.md) · [eBPF as a Future Extension](07-ebpf-future.md) →
