# P2.4 — Async Dependency Instrumentation & Context Propagation

**Status:** Complete. Empirical regression and deterministic cross-thread migration proof verified.
**Result:** The existing native P1.6 `FutureExt::with_context` implementation correctly preserves OpenTelemetry context across suspension, resumption, and cross-thread migration without production changes.

This record provides the evidence for [Issue #5](https://github.com/darkraider01/rust-compile-time-instrumentation-/issues/5).

---

## 1. Goal & Context

In an asynchronous Rust runtime, an instrumented dependency future may:
1. be instantiated under an application parent context;
2. poll initially on Thread A and suspend at an `.await` boundary (`Poll::Pending`);
3. be migrated by the executor (or transferred across threads) to Thread B;
4. resume on Thread B and poll to completion (`Poll::Ready`).

Under [ADR-011](../decision-records.md#adr-011---the-tier-2-c-abi-is-provisional), compatible third-party dependencies are instrumented via native OpenTelemetry injection (`--extern opentelemetry=<rlib>`). The native emitter (`NativeEmitter` in `transform.rs`) wraps async function bodies in:

```rust
let __otel_tracer = opentelemetry::global::tracer("<crate_name>");
let __otel_span = opentelemetry::trace::Tracer::span_builder(&__otel_tracer, "<fn_name>")
    .with_kind(opentelemetry::trace::SpanKind::Internal)
    .start(&__otel_tracer);
let __otel_cx = <opentelemetry::Context as opentelemetry::trace::TraceContextExt>::current_with_span(__otel_span);
opentelemetry::trace::FutureExt::with_context(async move {
    // function body
}, __otel_cx).await
```

### Critical Distinction: Future Creation vs. First Poll

In Rust, an `async fn` is fundamentally lazy: calling an `async fn` does not immediately execute its body; it merely constructs and returns an anonymous generator / future state machine.
Consequently, the span initialization, tracer lookup, and context capture shown above execute when the future is **first polled**, not when the function call returns the future.

The Issue #5 deterministic proof remains fully valid because the future is first polled on Thread A while the application parent context is attached (`parent_guard`). When that first poll occurs, `Tracer::start` sees the ambient parent context in Thread A's thread-local storage and correctly binds `parent_span_id`. `FutureExt::with_context` then stores `__otel_cx` inside the returned `WithContext` future wrapper, guaranteeing that all subsequent polls (including resumed polls on Thread B) execute within that span's context.

Understanding this distinction is critical for **Issue #6**: if an unpolled or spawned future is sent to a background runtime executor via `tokio::spawn`, its first poll occurs on an executor worker thread where the caller's TLS context is *not* active. Therefore, context must be captured synchronously at the `tokio::spawn` task-creation boundary before scheduling.

The objective of Issue #5 is to prove that once a future is created and instrumented, `FutureExt::with_context` preserves context across thread migration and satisfies all lifecycle guarantees without relying on thread-local ownership assumptions.

---

## 2. Deterministic Cross-Thread Migration Proof

Rather than relying on non-deterministic work-stealing schedulers or cooperative runtime yields (`tokio::task::yield_now()`), the regression fixture in `cargo-instrument/tests/r4_extern_injection_tests.rs` constructs a fully deterministic cross-thread execution test:

```rust
/// Compile-time generic bound asserting that the future itself implements Send.
/// Takes ownership of val to assert Send directly on the future type.
fn assert_is_send<T: Send>(val: T) -> T {
    val
}

fn test_cross_thread_migration<F, R>(
    parent_name: &'static str,
    make_fut: impl FnOnce() -> F + Send + 'static,
) -> (opentelemetry::trace::TraceId, opentelemetry::trace::SpanId)
where
    F: std::future::Future<Output = R> + Send + 'static,
    R: Send + 'static,
{
    let tracer = opentelemetry::global::tracer("r4-app");
    let parent_span = tracer.start(parent_name);
    let parent_cx = opentelemetry::Context::current_with_span(parent_span);
    let parent_trace_id = parent_cx.span().span_context().trace_id();
    let parent_span_id = parent_cx.span().span_context().span_id();

    let (tx_a_to_b, rx_b) = std::sync::mpsc::sync_channel::<Pin<Box<F>>>(1);

    // Thread A: create future under application parent context and poll once to Pending
    let parent_cx_for_a = parent_cx.clone();
    let thread_a = thread::Builder::new()
        .name("thread-a-poller".into())
        .spawn(move || {
            let parent_guard = parent_cx_for_a.attach();

            // Create future under active application parent context
            let fut = make_fut();
            // Statically assert Send on the future itself
            let fut = assert_is_send(fut);
            let mut pinned_fut = Box::pin(fut);

            // Poll once on Thread A
            let waker_a = create_waker();
            let mut task_cx_a = TaskContext::from_waker(&waker_a);
            let poll1 = pinned_fut.as_mut().poll(&mut task_cx_a);
            assert!(poll1.is_pending(), "first poll on Thread A must return Poll::Pending");

            // Drop parent guard and verify Thread A has no active context leak
            drop(parent_guard);
            assert_eq!(
                opentelemetry::Context::current().span().span_context().span_id(),
                opentelemetry::trace::SpanId::INVALID,
                "Thread A must have no active context leak after first poll and guard drop"
            );

            // Transfer the pending future across thread boundary to Thread B
            tx_a_to_b.send(pinned_fut).expect("send future to Thread B");
        })
        .expect("spawn Thread A");

    thread_a.join().expect("Thread A panicked");

    // Thread B: receive pending future, resume, and poll to Ready
    let thread_b = thread::Builder::new()
        .name("thread-b-poller".into())
        .spawn(move || {
            let mut pinned_fut = rx_b.recv().expect("receive future on Thread B");

            // Verify Thread B does not implicitly inherit any context
            assert_eq!(
                opentelemetry::Context::current().span().span_context().span_id(),
                opentelemetry::trace::SpanId::INVALID,
                "Thread B must have no active context before polling"
            );

            // Poll on Thread B until Poll::Ready
            let waker_b = create_waker();
            let mut task_cx_b = TaskContext::from_waker(&waker_b);
            let poll2 = pinned_fut.as_mut().poll(&mut task_cx_b);
            assert!(poll2.is_ready(), "resumed poll on Thread B must return Poll::Ready");

            // Verify Thread B has no active context leak after completion
            assert_eq!(
                opentelemetry::Context::current().span().span_context().span_id(),
                opentelemetry::trace::SpanId::INVALID,
                "Thread B must have no active context leak after future completion"
            );

            // Drop the completed future on Thread B
            drop(pinned_fut);

            // Verify Thread B remains clean after drop
            assert_eq!(
                opentelemetry::Context::current().span().span_context().span_id(),
                opentelemetry::trace::SpanId::INVALID,
                "Thread B must have no active context leak after dropping future"
            );
        })
        .expect("spawn Thread B");

    thread_b.join().expect("Thread B panicked");

    parent_cx.span().end();
    (parent_trace_id, parent_span_id)
}
```

---

## 3. Acceptance Evidence

The deterministic migration test was executed against both plain async dependency functions and `Result`-returning async functions (`async_result_work(true)`):

| Acceptance Criterion | Result | Evidence |
| --- | --- | --- |
| **Same Trace** | **Pass** | `dep_span.span_context.trace_id() == parent_trace_id` verified for both plain and Result shapes. |
| **Correct Parent Span** | **Pass** | `dep_span.parent_span_id == parent_span_id` verified across thread boundary. |
| **No Active Context Leaks** | **Pass** | Verified `Context::current().span().span_context().span_id() == SpanId::INVALID` on Thread A after suspension and on Thread B before and after completion. |
| **Thread-Identity Independence** | **Pass** | Initial poll executed on dedicated OS Thread A; resumption executed on dedicated OS Thread B. |
| **Preserve `Send` Trait** | **Pass** | Static compile-time bound `assert_is_send(fut)` and dynamic transfer over `sync_channel<Pin<Box<F>>>`. |
| **Single Span Completion** | **Pass** | Exactly one span exported per function invocation; suspension and resumption do not duplicate spans. |
| **P1.6 Semantic Compatibility** | **Pass** | Error status (`Status::error("")`) successfully captured on `Result` returns; ordinary direct async calls remain green. |

The test suite executed with:

```powershell
cargo test -p cargo-instrument --test r4_extern_injection_tests -- --ignored --nocapture
```

Test result: `test r4_extern_injection_path_dependency_proof ... ok (52.83s)`.

---

## 4. Production Code Impact

**Finding:** The existing native P1.6 `FutureExt::with_context` implementation in `transform.rs` already satisfies all requirements.

- `WithContext` attaches its stored `Context` into thread-local storage for the exact duration of each `poll` call, dropping the guard upon return.
- Because `Context` is `Clone + Send + Sync + 'static`, the wrapper future safely moves between OS threads.
- No production modifications to `transform.rs`, `otel-shim`, or the compiler wrapper were necessary.

---

## 5. Scope Boundary: `tokio::spawn` (Issue #6)

In this test, `tokio::spawn(async { dep_r4::async_work().await })` produces a span parented by `SpanId::INVALID`. This confirms that while `FutureExt::with_context` successfully propagates context across `.await` suspension and thread migration of an already-instrumented future, task-creation boundaries (`tokio::spawn`) do not inherit ambient context without explicit task instrumentation.

Because the spawned future's first poll happens asynchronously on a Tokio executor worker thread rather than on the calling thread, ambient caller context in thread-local storage is not active at first poll time. The parent context must therefore be captured synchronously at the `tokio::spawn` call site before scheduling the task.

Preserving context across `tokio::spawn` is a distinct problem scheduled for **Issue #6**.
