← [Final Recommendation](15-final-recommendation.md) · [Contents](../README.md) · [Architecture Decision Records](17-decision-records.md) →

---

## 16. Instrumentation Semantics Specification

**Status: normative for Phase 1.** Everything before this section is research; this section is the contract. It defines precisely what "automatic instrumentation" means for this tool — what a generated span *is*, when it starts and ends, what it is parented to, and what happens under async suspension, error, panic, cancellation, and concurrency.

**This document is the correctness oracle.** [§12.8](12-mvp-definition.md)'s test suite exists to verify the invariants below, and [§14.1](14-evaluation-plan.md) measures against them. A behaviour not specified here is not a bug when it differs between builds — it is an unspecified behaviour, and that distinction is deliberate ([§16.15](#1615-deliberately-unspecified-in-phase-1)).

**Reading convention.** **MUST** / **MUST NOT** are invariants a Phase 1 build has to satisfy to be considered correct. **SHOULD** is a default that a rule may override. Evidence tags follow the [README convention](../README.md); a shape marked **[Design]** is a decision made here and *not yet* experimentally validated.

---

### 16.1 Definitions

| Term | Meaning in this document |
| --- | --- |
| **Instrumentation site** | A single syntactic location — one function or method definition — that the rule engine matched and the splicer edited. One site per *definition*, never per monomorphized instantiation ([§6.2](06-rust-specific-challenges.md)) |
| **Invocation** | One runtime execution of an instrumented function body. For an `async fn`, one execution of the returned future to completion or drop — **not** one `poll()` |
| **Span** | An `opentelemetry::trace::Span` created by the runtime on behalf of a site |
| **Context** | An `opentelemetry::Context`, the ambient value the OTel SDK parents new spans against. Thread-local, per the OTel Rust API |
| **Attach / detach** | Making a `Context` current on the executing thread, and restoring the previous one. `Context::attach()` returns a guard whose `Drop` detaches |
| **Guard** | A generated RAII value whose `Drop` ends a span, detaches a context, or both |
| **Tier 1 / Tier 2** | Whether the instrumented crate may name the `opentelemetry` crate. See [§16.3](#163-the-two-emission-tiers) — this distinction is load-bearing and was not previously stated |

---

### 16.2 Universal invariants

These hold for every instrumented construct, in both tiers.

| # | Invariant |
| --- | --- |
| **S1** | **One span per invocation.** An instrumented function that is called once produces exactly one span, regardless of how many times its future is polled, how many return paths it has, or how many times it is inlined |
| **S2** | **A span's start precedes its end**, and its `[start, end]` interval covers the whole logical operation ([§16.7](#167-async-functions--the-normative-lifecycle) makes "whole" precise for async) |
| **S3** | **Parent = the context current at span creation.** The tool never constructs a parent relationship itself; it defers entirely to the SDK's context resolution |
| **S4** | **A span MUST be ended on every exit path** — normal return, early `return`, `?` propagation, and unwind. Ending is therefore always via `Drop`, never via a statement appended to the end of a body |
| **S5** | **No context outlives its invocation.** Every attach is paired with a detach on the same thread, in LIFO order. A generated site MUST NOT leave a context attached after the function returns or the future yields |
| **S6** | **MUST NOT hold an attach guard across an `.await`.** This is the classic Rust instrumentation bug ([R5](13-technical-risks.md), [§6.3](06-rust-specific-challenges.md)); it corrupts other tasks' traces globally, not locally |
| **S7** | **Argument and return values are never read.** Not formatted, not `Debug`-printed, not stored. This is a security property ([R17](13-technical-risks.md)) and a panic-safety property ([R18](13-technical-risks.md)), not a performance default |
| **S8** | **Instrumentation MUST NOT change program behaviour.** Same stdout, same return values, same error paths, same panics. Verified by [§12.8](12-mvp-definition.md)'s "instrumented and uninstrumented builds produce the same program output" |
| **S9** | **Instrumentation MUST NOT panic.** No allocation-failure paths, no unwrap on user data, no formatting of user types. A runtime that cannot start a span returns the null handle and the program continues untraced |
| **S10** | **Idempotence.** A site already instrumented — by us, or by hand with `#[tracing::instrument]` or an explicit OTel span — MUST NOT be instrumented again ([R10](13-technical-risks.md), [§12.9](12-mvp-definition.md) O5) |
| **S11** | **Fail open, per crate.** A crate that cannot be parsed, mirrored, or safely spliced is compiled **unmodified**, with the reason recorded in the plan. A missing span is always preferable to a broken build ([R2](13-technical-risks.md), [§6.11](06-rust-specific-challenges.md)) |
| **S12** | **The disabled build is byte-equivalent to no tool.** With the `--cfg` gate off, no call site is spliced at all, so configuration B in [§14.2](14-evaluation-plan.md) must equal the uninstrumented baseline. This is the only compile-time removal mechanism the native OTel API allows ([R23](13-technical-risks.md)) |

---

### 16.3 The two emission tiers

**[Design — this resolves a contradiction the previous documents did not notice.]**

[§12.1a](12-mvp-definition.md) specifies that an instrumented dependency receives **only** an `extern "C"` symbol declaration and **no** Cargo dependency edge — that is the whole point of the trampoline mechanism ([Appendix C.2](appendix-c-adversarial-review.md), [ADR-003](17-decision-records.md)). [§12.4](12-mvp-definition.md) specifies that async sites are wrapped with `opentelemetry::trace::FutureExt::with_context`.

**These two statements are incompatible for a dependency crate.** `with_context` is a trait method on a type from the `opentelemetry` crate; a crate that cannot name `opentelemetry` cannot call it. The `extern "C"` boundary carries no Rust futures.

The resolution is two emission tiers with **one shared semantics**:

| | **Tier 1 — the application / workspace crates** | **Tier 2 — third-party dependency crates** |
| --- | --- | --- |
| Can name `opentelemetry`? | Yes — it is the runtime owner and declares the dependency anyway ([§12.4](12-mvp-definition.md)) | **No** — no `--extern`, no `-L`, no manifest edit ([§12.1a](12-mvp-definition.md)) |
| Sync emission | Native API: `tracer.start(...)` + `Context::attach` guard | `__otel_span_enter` / `__otel_span_exit` trampolines |
| Async emission | **`FutureExt::with_context`**, literally | A spliced, `core`-only future wrapper reproducing the same lifecycle over the C ABI |
| Semantics | **Identical.** [§16.7](#167-async-functions--the-normative-lifecycle) is written against `with_context`'s behaviour, and Tier 2 is required to match it | |

**`FutureExt::with_context` is the normative reference.** Tier 1 emits it. Tier 2 emits source that must be observationally equivalent to it — the runtime, behind the C ABI, is the same OTel SDK either way.

**[Fact]** What the [Appendix C.2](appendix-c-adversarial-review.md) experiment actually demonstrated is a **synchronous** enter/exit call spliced into an undeclared dependency. **Tier-2 async has not been demonstrated**, and this document does not claim it has. It is scheduled as [Appendix E](appendix-e-experiment-matrix.md) FE-2 and scoped out of the Phase 1 MVP ([§12.3](12-mvp-definition.md)).

#### The trampoline ABI

**[Design — unvalidated beyond the sync pair.]** All symbols are `extern "C"`, take no Rust types, and are resolved at the application's final link ([§12.1a](12-mvp-definition.md)).

```c
/* handle 0 is the null span: instrumentation disabled, or the runtime declined.
   Every function MUST accept handle 0 and do nothing. (S9) */

uint64_t __otel_span_enter(const uint8_t *name, uintptr_t name_len,
                           const uint8_t *file, uintptr_t file_len,
                           uint32_t line, uint8_t kind);   /* start + attach   — sync sites  */
void     __otel_span_exit (uint64_t handle);               /* detach + end     — sync sites  */

uint64_t __otel_span_start(const uint8_t *name, uintptr_t name_len,
                           const uint8_t *file, uintptr_t file_len,
                           uint32_t line, uint8_t kind);   /* start, NOT attached — async    */
void     __otel_span_end  (uint64_t handle);               /* end                 — async    */
uint64_t __otel_ctx_attach(uint64_t handle);               /* per poll()          — async    */
void     __otel_ctx_detach(uint64_t token);                /* per yield           — async    */

void     __otel_span_set_error(uint64_t handle);           /* status = Error      — §16.10   */
```

- All string pointers refer to `&'static str` data spliced as literals; no allocation crosses the boundary.
- `kind` encodes `SpanKind` (`0 = Internal`, `1 = Server`, `2 = Client`, …), first-class in the native API and one of the reasons for [ADR-001](17-decision-records.md).
- **Deferred optimization:** passing the name/file/line tuple on every invocation is wasteful; a site-registration call returning a site id, made once per site, is the obvious improvement. Not Phase 1 — measure first ([§14.2](14-evaluation-plan.md)).

#### Two constraints on spliced code that the earlier documents missed

**[Fact]** Calling an `extern "C"` function is an unsafe operation, and a crate carrying `#![forbid(unsafe_code)]` **cannot** be given spliced trampoline calls — `forbid` cannot be lifted by an inner `#[allow]`, so the splice is a hard compile error (`E0453`). A non-trivial share of the ecosystem uses this attribute. Two consequences:

1. **Phase 1 behaviour: skip the crate** and record the reason in the plan (S11). Stripping the user's own safety lint to instrument them is not an acceptable alternative.
2. **A possible escape, to be tested, not assumed.** Rust 1.82+ allows individual items in an `unsafe extern` block to be declared `safe fn`, which are then callable without an `unsafe` block. Whether that also avoids tripping the `unsafe_code` lint is **[Open question]** — scheduled as [Appendix E](appendix-e-experiment-matrix.md) FE-3, a cheap experiment with a material coverage payoff.

**[Fact]** `unsafe extern "C" { … }` is edition-2024 syntax; older editions require a bare `extern "C" { … }` block. The wrapper already receives `--edition` in its argv ([§3.3](03-rust-compiler-pipeline.md)), so the splicer **MUST** select the declaration form per crate edition rather than emitting one shape everywhere ([R4](13-technical-risks.md)).

---

### 16.4 Synchronous functions

**Semantics.** The span starts when the function body begins executing and ends when control leaves the body by any path. Its context is current for exactly that interval, so any instrumented call made from the body is its child (S3).

**Generated shape** (Tier 1; non-normative illustration — only the semantics are normative):

```rust
fn handle(req: Request) -> Response {
    let __otel = __otel_rt::enter("my_crate::handle", file!(), line!(), Kind::Internal);
    /* original body, byte-identical, spliced in below the guard */
}
```

- The guard is the **first** statement of the body, so the span covers argument-destructuring side effects that occur inside the body but not the caller's argument evaluation.
- The guard is bound to a named local, never `let _ = …`. **[Fact]** `let _ =` drops immediately, which would end the span before the body runs — a bug worth naming because it is easy to introduce and invisible in review.
- `Drop` order guarantees S4: the guard is the first local declared, so it is the last dropped, on every path including unwind.

---

### 16.5 Methods and trait implementations

Same lifecycle as [§16.4](#164-synchronous-functions). Only naming differs ([§16.14](#1614-naming-and-attributes)):

| Site | Span name |
| --- | --- |
| Inherent method | `my_crate::Foo::method` |
| Trait impl method | `<my_crate::Foo as my_crate::Trait>::method` |
| Trait *default* body | Not instrumented in Phase 1 — instrument `impl` blocks, not trait definitions ([§6.2](06-rust-specific-challenges.md)) |

**MUST NOT** read `self` (S7). A method's receiver is a value like any other.

---

### 16.6 Generic functions

**One site per definition**, not per instantiation ([§6.2](06-rust-specific-challenges.md)). All monomorphizations of `fn parse<T>()` therefore share one span name, `my_crate::parse`.

- Recording the concrete type via `std::any::type_name::<T>()` is cheap and produces a `&'static str`, but it multiplies span-name cardinality by the instantiation count. **SHOULD** be off in Phase 1; available as a per-rule opt-in.
- **Consequence to document rather than fix:** a hot generic instantiated many ways is one site and potentially millions of invocations ([R7](13-technical-risks.md)). The size threshold and exclusion rules are the mitigation.

---

### 16.7 Async functions — the normative lifecycle

This is the section the MVP is judged against.

**[Fact, [Appendix D.2](appendix-d-maintainer-qa.md)]** `opentelemetry::trace::FutureExt::with_context(cx)` wraps any `Future`, attaching `cx` at the start of each `poll()` and detaching when the future yields. That behaviour is the specification below; Tier 2 must reproduce it ([§16.3](#163-the-two-emission-tiers)).

**The required sequence for one invocation:**

| Event | Required behaviour |
| --- | --- |
| Future **constructed** (the `async fn` is called) | **No span is started.** [Design — see below] |
| **First `poll()`** | Span starts. Its parent is the context current *on the polling thread at that moment* (S3). Context attached for the duration of the poll |
| Poll **returns `Pending`** (the task suspends) | Context detached. **The span remains open and its clock keeps running.** The context MUST NOT be current on that thread afterwards (S5) |
| **Subsequent `poll()`**, possibly on a different worker thread | Context attached again, on that thread. Still the same single span (S1) |
| Poll **returns `Ready`** | Context detached; span ends |
| Future **dropped before completion** | Span ends. See [§16.12](#1612-cancellation) |

**Consequences that are testable, and are the MVP's success criteria ([§12.7](12-mvp-definition.md)):**

- **Exactly one span** for a future polled N times — not N (S1).
- **Duration ≈ wall-clock** of the logical operation: an `async fn` awaiting a 100 ms sleep yields a span of ≈100 ms, not ≈0 ms of CPU-busy time.
- **No busy/idle split.** `tracing-opentelemetry` synthesises that from enter/exit pairs; the OTel data model has no field for it ([§5.3](05-otel-rust.md)). Its absence is a diagnostic loss, not an incorrectness.
- **Suspension does not leak.** While task A is suspended, an unrelated instrumented function running on the same worker thread MUST NOT become a child of A's span. This is the sharpest single test of `with_context` and the one that catches S6 violations.
- **Migration is invisible.** A task moved between worker threads mid-`await` produces one coherent span, not two.

#### The one genuinely open semantic decision: when does the span start?

**[Design — deliberate, and flagged for validation.]** The table above starts the span at **first poll**, not at future construction.

- **Why:** Rust futures are lazy. `let fut = f(); expensive(); fut.await` would otherwise attribute all of `expensive()` to `f`'s span, and a future that is constructed and never awaited would produce a span for work that never happened.
- **The cost:** queueing delay between construction and first poll is invisible. For a spawned task this is arguably information a user wants.
- **The alternative** — start at construction — is simpler to generate but reports durations that are wrong in exactly the way [§5.4](05-otel-rust.md) accused the naive OTel approach of being wrong.
- **Validation:** [Appendix E](appendix-e-experiment-matrix.md) FE-4 — construct a future, sleep 100 ms, then await it; assert the span's duration excludes the sleep. Add to the MVP suite.

---

### 16.8 Nested calls and parenting

- A call from an instrumented body to another instrumented function produces a child span, because the caller's context is current (S3).
- A call **through** uninstrumented code (an excluded function, a closure, a macro-generated body) still produces a child of the nearest enclosing instrumented span. The context is thread-local and does not care what is on the stack between them. **The span tree is therefore a subsequence of the call tree, never a distortion of it.**
- Crossing `tokio::spawn` **breaks the chain in Phase 1**: a spawned task starts with a fresh context ([§6.4](06-rust-specific-challenges.md)). This is a documented limitation, not a defect, and is the first post-MVP feature.

---

### 16.9 Recursion

- **Directly self-recursive functions are excluded by default** ([§12.3](12-mvp-definition.md)), detected syntactically. A depth-*N* recursion would otherwise produce *N* nested spans and destroy the trace ([§6.8](06-rust-specific-challenges.md)).
- **Mutual recursion is not detected**, and MUST be documented as such. If instrumented, the semantics are simply the ordinary nesting rule applied repeatedly — correct, but potentially enormous.
- If a rule explicitly opts a recursive function in, each level is one span, correctly nested. No depth guard in Phase 1.

---

### 16.10 Errors and `Result`

- A function returning `Result<T, E>` **SHOULD** set span status `Error` when the value returned is `Err`, and leave the status unset otherwise (`record_error: true` in the rule vocabulary, [§12.6](12-mvp-definition.md)).
- **The error value MUST NOT be formatted, stored, or transmitted** (S7). Status is set with **no description**. A `Display`/`Debug` impl on a user error type may allocate, may panic, and may contain credentials — all three are unacceptable inside injected code ([R17](13-technical-risks.md), [R18](13-technical-risks.md)).
- The check is a discriminant read on a borrowed value, made before the value is returned; the returned value MUST NOT be moved or cloned to perform it (S8).
- `?`-propagation is an early return and is covered by S4. It is not separately detectable and produces no distinct signal.
- **Not covered in Phase 1:** functions returning `Option`, custom result-like types, or error-carrying enums. Only `Result<_, _>` written syntactically at the site.

---

### 16.11 Panics and unwinding

| Configuration | Behaviour |
| --- | --- |
| `panic = "unwind"` (default) | Guards drop during unwind, so **the span ends** and is exported. Its status is **not** set to `Error` in Phase 1 — a panicking span is indistinguishable from a short successful one, and this MUST be documented |
| `panic = "abort"` | `Drop` does not run. **The span never ends and is never exported.** Acceptable: the process is dying ([§6.6](06-rust-specific-challenges.md)) |

`std::panic::catch_unwind` is **not** used. It is not free, it is a no-op under `panic=abort`, and swallowing a user's panic would violate S8.

---

### 16.12 Cancellation

A future dropped before completion — `select!`, a timeout, a dropped `JoinHandle` — is a normal and frequent event in async Rust, not an error.

- **The span MUST end when the future is dropped** (S4). This is why the span-ending mechanism is a `Drop` guard *inside* the future rather than a statement at the end of the body: a body that never finishes never reaches its own last statement.
- **Phase 1: a cancelled span is not distinguished from a completed one.** Status unset, no attribute.
- **Deferred, with the mechanism already known:** a `bool` set on the body's normal-completion path and read in `Drop` distinguishes the two for the cost of one stack byte. Worth doing once there is evidence users need it; not an MVP requirement.

---

### 16.13 Concurrency

- *K* concurrently executing instrumented async functions produce *K* independent, non-interleaved span trees.
- Concurrency correctness follows entirely from S5 and S6: if every attach is paired with a detach at the yield boundary, no task can observe another's context. There is no lock, no registry, and no shared state in generated code.
- **Worker-thread migration mid-`await` is explicitly in scope** ([§12.7](12-mvp-definition.md) criterion 5, [Appendix D.6](appendix-d-maintainer-qa.md) D-Q1). It is the case `with_context`'s attach-per-poll design exists to handle, and therefore the case worth proving rather than assuming.

---

### 16.14 Naming and attributes

**Span name:** the fully-qualified path of the definition — `crate::module::function`, or `<Type as Trait>::method` for trait impls. Rule-overridable via the `name` template ([§12.6](12-mvp-definition.md)).

**Span kind:** `Internal` by default. `Server`/`Client` come from library-specific rules, which are Phase 2. First-class in the native API — one of the capabilities [ADR-001](17-decision-records.md) restored.

**Attributes** — **[Fact]** stable since semconv v1.33.0 ([§5.3](05-otel-rust.md)):

| Attribute | Value |
| --- | --- |
| `code.function.name` | Fully-qualified function name, without arguments |
| `code.file.path` | Source path of the **original** file, not the mirrored/spliced copy |
| `code.line.number` | Line of the original definition |

`code.column.number` and `code.stacktrace` are not emitted in Phase 1. **No semantic-convention compliance is claimed beyond the `code.*` group** — there is no convention for "function `foo::bar` was called" ([§5.3](05-otel-rust.md)).

**`code.file.path` and `code.line.number` MUST refer to the original source.** The splicer knows both, since it computed the byte range in the original buffer ([ADR-002](17-decision-records.md)). Reporting a location inside the mirrored tree would make every span unclickable and is the kind of detail that destroys trust in a tool.

---

### 16.15 Deliberately unspecified in Phase 1

Naming these prevents them being read as defects, and prevents them being quietly assumed to work.

| Area | Status |
| --- | --- |
| Argument and return values | Never captured (S7). Opt-in, per-rule, post-MVP |
| Panic → span status | Span closes; status unset ([§16.11](#1611-panics-and-unwinding)) |
| Cancellation → span status | Span closes; not distinguished ([§16.12](#1612-cancellation)) |
| `tokio::spawn` context propagation | Chain breaks; first post-MVP feature ([§6.4](06-rust-specific-challenges.md)) |
| Cross-process W3C propagation | Phase 2+; requires library-specific rules |
| Span links | Available in the API ([ADR-001](17-decision-records.md)) but unused in Phase 1. The natural first use is spawn fan-out |
| Macro-generated functions | Invisible to a source-level tool; never instrumented, never claimed ([§6.7](06-rust-specific-challenges.md)) |
| `std` and precompiled crates | Out of scope; needs `-Z build-std` (nightly) |
| Async functions **inside a dependency** | Tier-2 async is unproven; sync-only in the Phase 1 dependency slice ([§16.3](#163-the-two-emission-tiers)) |
| Sampling | Entirely the SDK's concern. The tool generates spans and never decides whether they are recorded |

---

### 16.16 Oracle: invariant → test

Every invariant is testable, and this table is the mapping [§12.8](12-mvp-definition.md) implements.

| Invariant | Test |
| --- | --- |
| S1 (one span per invocation) | Async fn polled N times → exporter received exactly 1 span |
| S2 / [§16.7](#167-async-functions--the-normative-lifecycle) (duration) | `await` a 100 ms sleep → span duration ≈ 100 ms |
| S3 / [§16.8](#168-nested-calls-and-parenting) (parenting) | Sync chain `a → b → c` → three correctly nested spans |
| S4 (ended on every path) | Early `return`, `?`-propagation, and unwind each produce a closed span |
| S5 / S6 (no leak across suspension) | While task A sleeps, an unrelated instrumented fn on that worker thread is **not** a child of A |
| S6 / [§16.13](#1613-concurrency) (isolation) | 10 concurrent tasks → 10 independent trees, including across worker-thread migration |
| S7 (no value capture) | Instrument a fn whose argument type has a panicking `Debug` impl → no panic, no attribute |
| S8 (behaviour preserved) | Instrumented and uninstrumented binaries produce identical program output |
| S9 (no panic) | Runtime returns null handle → program runs untraced, does not crash |
| S10 (idempotence) | A fn already carrying `#[instrument]` or a hand-written OTel span is not double-instrumented |
| S11 (fail open) | An unparseable / `forbid(unsafe_code)` crate compiles unmodified and is listed in the plan with a reason |
| S12 (gate off ≡ baseline) | `--cfg` gate off → binary size and benchmark equal to configuration A ([§14.2](14-evaluation-plan.md)) |
| [§16.10](#1610-errors-and-result) (errors) | `Err` return → span status `Error`, **no** description attribute |
| [§16.11](#1611-panics-and-unwinding) (panic) | Panic under `panic=unwind` → span closed, status unset |
| [§16.12](#1612-cancellation) (cancellation) | Future dropped mid-`await` → span closed |
| [§16.14](#1614-naming-and-attributes) (attributes) | `code.file.path` / `code.line.number` point at the **original** source, not the mirror |

---

### 16.17 Open semantic questions

| # | Question | Where it is tracked |
| --- | --- | --- |
| SQ1 | Does span-start-at-first-poll match user expectation, or is construction-to-first-poll delay information worth keeping? | [Appendix E](appendix-e-experiment-matrix.md) FE-4 |
| SQ2 | Can a `core`-only spliced future wrapper reproduce `with_context`'s lifecycle over the C ABI, across arbitrary dependency crates and editions? | [Appendix E](appendix-e-experiment-matrix.md) FE-2; the largest unknown in the semantics |
| SQ3 | Does per-poll attach/detach through the C ABI cost materially more than in-crate `with_context`? | [Appendix E](appendix-e-experiment-matrix.md) FE-6 |
| SQ4 | Does `unsafe extern { safe fn … }` (Rust 1.82+) permit instrumenting `#![forbid(unsafe_code)]` crates? | [Appendix E](appendix-e-experiment-matrix.md) FE-3 |
| SQ5 | Should a cancelled span be distinguished from a completed one before users ask? | Deferred ([§16.12](#1612-cancellation)) |

---

---

← [Final Recommendation](15-final-recommendation.md) · [Contents](../README.md) · [Architecture Decision Records](17-decision-records.md) →
