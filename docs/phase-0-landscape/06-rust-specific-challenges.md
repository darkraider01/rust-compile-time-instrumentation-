← [OpenTelemetry Rust](05-otel-rust.md) · [Contents](README.md) · [eBPF as a Future Extension](07-ebpf-future.md) →

---

## 6. Rust-Specific Instrumentation Challenges


For each construct: what makes it hard, and at which layer(s) it is tractable.

### 6.1 Ordinary functions

Easy at every layer. Source: add `#[instrument]`. MIR: prepend a call at the entry block, append at every `Return` terminator. Binary: uprobe on the symbol.

**Complication:** every layer must decide what to *name* the span and which arguments to record. Recording all arguments (the `#[instrument]` default) is wrong for automatic instrumentation — it can leak secrets and it can be expensive (`Debug`-formatting a large struct on every call). **Automatic instrumentation must default to `skip_all`.** This is a security requirement, not a performance preference: an auto-instrumentation tool that records every argument by default will exfiltrate passwords, tokens, and PII into a telemetry backend without anyone deciding to.

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
| **Source (`#[instrument]`)** | Wrap the returned future in `Instrumented`, entering/exiting the span around each `poll` | **Correct.** Span opens when the future is first polled, closes when it completes. `tracing` separates busy time (in `poll`) from idle time (suspended) |
| **Source (naïve — RAII guard at the top of the body)** | `let _g = span.enter();` as the first statement of an `async fn` | **Wrong, and this is the classic Rust tracing bug.** The guard is held across `.await`, so the span stays "entered" while the task is suspended and another task runs — corrupting the thread-local span stack. `tracing` documents this hazard; `#[instrument]` exists partly to prevent it |
| **MIR on the outer `foo` body** | Instrument entry/exit of the function that *constructs* the future | **Meaningless.** Measures the nanoseconds spent building a struct. The actual work has not happened yet, and may never happen if the future is dropped unpolled |
| **MIR on the coroutine body, post-`StateTransform`** | Instrument entry/exit of the `poll` function | **One span per poll.** You get N spans for a future polled N times, none of which corresponds to the logical operation |
| **MIR on the coroutine body, pre-`StateTransform`** | Instrument the pre-transform body, letting `StateTransform` split it | **Potentially correct, and this is the interesting research question.** Instrumentation inserted before the transform would be threaded through the state machine by the transform itself. But the span guard would become a local live across suspension points, hence a field of the coroutine struct — which is *exactly* the right representation, and is essentially what `Instrumented` achieves by hand |
| **LLVM / binary (uprobe on `foo`)** | Probe the symbol `foo` | Measures future construction. Meaningless, as above |
| **LLVM / binary (uprobe on `poll`)** | Probe the poll function | One event per poll, no logical operation boundary, and the poll symbol is hard to attribute back to the source `async fn` |

**[Inference — the most important technical conclusion in this document]** Async Rust instrumentation is *easier* at the source layer than at any lower layer, because the source layer is the only one where the logical operation still exists as a single entity. Every layer below has already split it into "construct" and "poll N times."

**[Hypothesis — worth testing, do not assume]** MIR instrumentation inserted into a coroutine body *before* `StateTransform` would be correctly threaded into the state machine, producing a span guard stored in the coroutine struct and thus correct logical-duration semantics. This would require overriding `mir_drops_elaborated_and_const_checked` (which rustc explicitly supports overriding, per the `#114628` comment **[Fact]**) and running the pass at the right point in `run_analysis_to_runtime_passes` rather than overriding `optimized_mir`. If true, this is a genuinely novel and defensible technical result. If false — for example if the borrow checker or the transform rejects our injected locals — MIR-level async instrumentation is a dead end and Architecture B loses most of its appeal.

**MVP:** async functions **must** be supported (they are the majority of code in any real Rust service), via `#[instrument]`-equivalent generation at source level. MIR-level async is explicitly deferred to a research spike.

### 6.4 Futures, `.await`, Tokio tasks, spawned tasks

**`.await` points.** Instrumenting individual `.await` expressions (rather than function boundaries) would give await-level granularity. `tracing` has no idiomatic construct for this beyond `.instrument(span)` on the awaited future. **[Inference]** High cardinality, low value. Defer indefinitely.

**`tokio::spawn`.** A spawned task starts with a *fresh* context — `tracing`'s span context is thread-local and does not follow a future onto another worker thread unless the future was explicitly `.instrument(...)`-ed before being spawned. This is the single most common cause of "my trace is broken across a spawn" in Rust.

**[Inference]** A `wrap_call`-style rule that rewrites `tokio::spawn(fut)` into `tokio::spawn(fut.instrument(tracing::Span::current()))` would fix a real, common, well-known bug class automatically. It is a strong candidate for the *second* rule we implement after generic function instrumentation, and it is a good demonstration of why call-site rules (not just definition rules) are needed. It is also a good argument for copying `otelc`'s `wrap_call` rule type rather than only `inject_hooks`.

**[Open question]** Does `#[instrument]` on an `async fn` that internally spawns propagate to the spawned task? No — the spawned future is a separate future. Confirm with an experiment in Phase 1's test suite so we can document the limitation precisely.

**MVP:** spawn propagation is **out** of the MVP but should be the first post-MVP feature.

### 6.5 Closures

Closures have no name, may be inlined trivially, and are frequently tiny (`|x| x + 1`). Instrumenting them automatically is almost always wrong: enormous span volume, no semantic value.

**[Inference]** Exclude closures by default at every layer. Provide opt-in via a directive-style rule if anyone ever asks.

**MVP:** excluded.

### 6.6 Panics and unwinding

A span guard is an RAII value; its `Drop` runs during unwind, so the span closes correctly on panic **[Inference]** — but nothing records *that* it panicked, so a panicking span looks like a successful one that happened to be short.

**[Fact]** `otelc` puts panic isolation in its trampoline specifically so that instrumentation cannot break the application. Our equivalent concerns:

- Under `panic = "abort"` (common in release profiles for size), `Drop` does not run during a panic at all, so the span never closes. **[Inference]** Acceptable — the process is dying.
- Instrumentation code itself must not panic. A `Debug` impl that panics, invoked while recording an argument, would turn a working program into a crashing one. This is the strongest argument for `skip_all` by default and for never formatting user values without an explicit rule saying to.
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

**[Fact]** `#[instrument]` cannot be applied to `const fn` — it is a compile error. Also to exclude: functions in `const` contexts, `#[no_std]` crates without an allocator, `build.rs` scripts, proc-macro crates, and test harness code.

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
| `tokio::spawn` context propagation | ⬜ first post-MVP feature | High value, needs call-site rules |
| Cross-process context propagation | ⬜ deferred | Needs library-specific rules |
| Argument recording | ❌ off by default | Security: PII/secret exfiltration risk |
| Panic → span status | ⬜ deferred | Needs unwind-aware hook |
| `std` instrumentation | ❌ out of scope | Needs `-Z build-std` |

---

---

← [OpenTelemetry Rust](05-otel-rust.md) · [Contents](README.md) · [eBPF as a Future Extension](07-ebpf-future.md) →
