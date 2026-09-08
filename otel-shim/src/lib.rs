//! Runtime trampoline shim providing `extern "C"` OpenTelemetry symbols
//! for compile-time instrumented dependency crates (Milestone P1.7).
//!
//! Conforms strictly to normative Phase 0 specifications (§16.3, ADR-002, ADR-003):
//! - All symbols use the C ABI (`extern "C"`), passing static pointers/primitives.
//! - Handle `0` is the null span / no-op (S9).
//! - Thread-local LIFO stack enforces strict single-thread context attachment and detachment (S5).
//! - Calling [`init`] in application code satisfies ADR-003 / E-10, preventing `rustc` extern-crate pruning.

use std::cell::RefCell;
use std::slice;
use std::str;

use std::sync::atomic::{AtomicU64, Ordering};

use opentelemetry::{Context, ContextGuard};

thread_local! {
    /// Thread-local LIFO stack of active spans and context guards.
    ///
    /// Stores `(handle, context, guard)` where:
    /// - `handle`: 64-bit identifier assigned by `__otel_span_enter`.
    /// - `context`: `Context` containing the active span, allowing handle-accurate error recording.
    /// - `guard`: `ContextGuard` keeping the span active in thread-local storage until dropped.
    static STACK: RefCell<Vec<(u64, Context, ContextGuard)>> = const { RefCell::new(Vec::new()) };
}

/// Global monotonic atomic counter for span handles.
/// Non-zero values only; 0 is reserved for null/disabled spans (S9).
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// Explicit entry-point reference for application code (ADR-003 / E-10).
///
/// Calling this function in application code creates a genuine Rust item path into `otel-shim`,
/// guaranteeing that `rustc` preserves `libotel_shim.rlib` on the linker command line.
pub fn init() {
    // Intentionally a lightweight no-op; existence of the item-path reference is the load-bearing mechanism.
}

/// Returns the number of currently active spans on the calling thread.
/// Primarily used by integration test fixtures to verify S5 context stack cleanup.
pub fn active_span_count() -> usize {
    STACK.with(|s| s.borrow().len())
}

/// Start an OpenTelemetry span and attach its context on the current thread.
///
/// # Safety
///
/// - `name` must be a non-null, valid pointer to a UTF-8 encoded byte sequence of length `name_len`.
/// - `file` must be a non-null, valid pointer to a UTF-8 encoded byte sequence of length `file_len`.
/// - The caller contract guarantees `name` and `file` refer to immutable string literals with `'static` lifetime spliced by the compiler wrapper.
#[no_mangle]
pub unsafe extern "C" fn __otel_span_enter(
    name: *const u8,
    name_len: usize,
    _file: *const u8,
    _file_len: usize,
    _line: u32,
    _kind: u8,
) -> u64 {
    if name.is_null() || name_len == 0 {
        return 0; // S9: Handle 0 is null/no-op
    }

    let name_bytes = slice::from_raw_parts(name, name_len);
    let name_str = match str::from_utf8(name_bytes) {
        Ok(s) => s,
        Err(_) => return 0,
    };

    let tracer = opentelemetry::global::tracer("dependency");
    let span = opentelemetry::trace::Tracer::span_builder(&tracer, name_str)
        .with_kind(opentelemetry::trace::SpanKind::Internal)
        .start(&tracer);

    let cx = <Context as opentelemetry::trace::TraceContextExt>::current_with_span(span);
    let guard = cx.clone().attach();

    let mut handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    if handle == 0 {
        handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    }

    STACK.with(|s| {
        s.borrow_mut().push((handle, cx, guard));
    });

    handle
}

/// Detach and complete an OpenTelemetry span previously started by [`__otel_span_enter`].
///
/// # Safety
///
/// `handle` must be either 0 (no-op) or a valid handle previously returned by `__otel_span_enter`
/// on the current thread.
#[no_mangle]
pub unsafe extern "C" fn __otel_span_exit(handle: u64) {
    if handle == 0 {
        return; // S9
    }

    let popped = STACK.with(|s| {
        let mut st = s.borrow_mut();
        // LIFO enforcement: pop if and only if handle matches the top of the stack
        if st.last().map(|(h, _, _)| *h) == Some(handle) {
            st.pop()
        } else {
            None
        }
    });
    drop(popped);
}

/// Record an error status on an OpenTelemetry span identified by `handle`.
///
/// Per §16.10, the error is recorded with an empty description (`Status::error("")`) to prevent
/// allocations, panics, or credential leaks in user `Debug` implementations.
///
/// # Safety
///
/// `handle` must be either 0 (no-op) or a valid handle previously returned by `__otel_span_enter`
/// on the current thread.
#[no_mangle]
pub unsafe extern "C" fn __otel_span_set_error(handle: u64) {
    if handle == 0 {
        return; // S9
    }

    STACK.with(|s| {
        let st = s.borrow();
        if let Some((_, cx, _)) = st.iter().rev().find(|(h, _, _)| *h == handle) {
            opentelemetry::trace::TraceContextExt::span(cx)
                .set_status(opentelemetry::trace::Status::error(""));
        }
    });
}

// ----------------------------------------------------------------------------
// Tier-2 Async Trampoline Stubs (Phase 2 / FE-13)
// ----------------------------------------------------------------------------

/// Start an async OpenTelemetry span without attaching context.
///
/// # Safety
///
/// `name` and `file` must be valid UTF-8 string pointers or null.
#[no_mangle]
pub unsafe extern "C" fn __otel_span_start(
    _name: *const u8,
    _name_len: usize,
    _file: *const u8,
    _file_len: usize,
    _line: u32,
    _kind: u8,
) -> u64 {
    0 // Handle 0 no-op stub: Tier-2 async deferred to Phase 2 (FE-13)
}

/// End an async OpenTelemetry span.
///
/// # Safety
///
/// `handle` must be either 0 or a valid handle returned by `__otel_span_start`.
#[no_mangle]
pub unsafe extern "C" fn __otel_span_end(_handle: u64) {
    // Handle 0 no-op stub: Tier-2 async deferred to Phase 2 (FE-13)
}

/// Attach an async span context for the duration of a `poll()`.
///
/// # Safety
///
/// `handle` must be either 0 or a valid handle returned by `__otel_span_start`.
#[no_mangle]
pub unsafe extern "C" fn __otel_ctx_attach(_handle: u64) -> u64 {
    0 // Token 0 no-op stub: Tier-2 async deferred to Phase 2 (FE-13)
}

/// Detach an async span context when yielding `Poll::Pending`.
///
/// # Safety
///
/// `token` must be either 0 or a token returned by `__otel_ctx_attach`.
#[no_mangle]
pub unsafe extern "C" fn __otel_ctx_detach(_token: u64) {
    // Token 0 no-op stub: Tier-2 async deferred to Phase 2 (FE-13)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
    use std::sync::OnceLock;

    static EXPORTER: OnceLock<InMemorySpanExporter> = OnceLock::new();

    fn get_test_exporter() -> InMemorySpanExporter {
        EXPORTER
            .get_or_init(|| {
                let exp = InMemorySpanExporter::default();
                let provider = SdkTracerProvider::builder()
                    .with_simple_exporter(exp.clone())
                    .build();
                opentelemetry::global::set_tracer_provider(provider);
                exp
            })
            .clone()
    }

    /// S9: Handle 0 no-op invariant.
    ///
    /// `__otel_span_exit(0)`, `__otel_span_set_error(0)`, and each async stub with handle 0
    /// must not panic and must produce no state change on either an active or empty stack.
    #[test]
    fn test_s9_handle_zero_is_noop() {
        init(); // Satisfy entry-point invariant
        assert_eq!(active_span_count(), 0);
        let name = b"active_span";
        let file = b"src/lib.rs";

        unsafe {
            // Push one span so we verify that handle 0 operations cause no state change on an active stack
            let h = __otel_span_enter(name.as_ptr(), name.len(), file.as_ptr(), file.len(), 1, 0);
            assert_ne!(h, 0);
            assert_eq!(active_span_count(), 1);

            // S9: Handle 0 no-op calls must not panic and must cause no state change
            __otel_span_exit(0);
            assert_eq!(active_span_count(), 1, "exit(0) must not change stack");

            __otel_span_set_error(0);
            assert_eq!(active_span_count(), 1, "set_error(0) must not change stack");

            // Async stubs with handle 0
            assert_eq!(
                __otel_span_start(std::ptr::null(), 0, std::ptr::null(), 0, 0, 0),
                0
            );
            __otel_span_end(0);
            assert_eq!(__otel_ctx_attach(0), 0);
            __otel_ctx_detach(0);

            assert_eq!(
                active_span_count(),
                1,
                "async stubs with 0 must not change stack"
            );

            // Clean up
            __otel_span_exit(h);
            assert_eq!(active_span_count(), 0);

            // Also verify on empty stack: no panic, no state change
            __otel_span_exit(0);
            __otel_span_set_error(0);
            __otel_span_end(0);
            assert_eq!(__otel_ctx_attach(0), 0);
            __otel_ctx_detach(0);
            assert_eq!(active_span_count(), 0);
        }
    }

    /// S5 / LIFO: Enter A, enter B, then exit(A) out of order -> must NOT pop (A isn't top).
    ///
    /// Then exit(B), exit(A) -> stack empty.
    #[test]
    fn test_s5_lifo_out_of_order_exit() {
        assert_eq!(active_span_count(), 0);
        let name_a = b"span_a";
        let name_b = b"span_b";
        let file = b"src/lib.rs";

        unsafe {
            let handle_a = __otel_span_enter(
                name_a.as_ptr(),
                name_a.len(),
                file.as_ptr(),
                file.len(),
                10,
                0,
            );
            assert_ne!(handle_a, 0);
            assert_eq!(active_span_count(), 1);

            let handle_b = __otel_span_enter(
                name_b.as_ptr(),
                name_b.len(),
                file.as_ptr(),
                file.len(),
                20,
                0,
            );
            assert_ne!(handle_b, 0);
            assert_ne!(handle_a, handle_b);
            assert_eq!(active_span_count(), 2);

            // Out-of-order exit: attempt to exit A while B is on top of the stack
            __otel_span_exit(handle_a);
            assert_eq!(
                active_span_count(),
                2,
                "Out-of-order exit(A) must NOT pop because A is not the top span"
            );

            // Exit B (which is top) -> must pop B
            __otel_span_exit(handle_b);
            assert_eq!(active_span_count(), 1, "exit(B) must pop top span B");

            // Now A is top -> exit(A) must pop A
            __otel_span_exit(handle_a);
            assert_eq!(
                active_span_count(),
                0,
                "exit(A) must now pop A, leaving stack empty"
            );
        }
    }

    /// Nesting: enter/enter/enter -> exit x 3 -> active_span_count() == 0.
    #[test]
    fn test_nesting_enter_exit() {
        assert_eq!(active_span_count(), 0);
        let file = b"src/lib.rs";

        unsafe {
            let h1 = __otel_span_enter(b"nest_1".as_ptr(), 6, file.as_ptr(), file.len(), 1, 0);
            assert_ne!(h1, 0);
            assert_eq!(active_span_count(), 1);

            let h2 = __otel_span_enter(b"nest_2".as_ptr(), 6, file.as_ptr(), file.len(), 2, 0);
            assert_ne!(h2, 0);
            assert_eq!(active_span_count(), 2);

            let h3 = __otel_span_enter(b"nest_3".as_ptr(), 6, file.as_ptr(), file.len(), 3, 0);
            assert_ne!(h3, 0);
            assert_eq!(active_span_count(), 3);

            // Exit in LIFO order
            __otel_span_exit(h3);
            assert_eq!(active_span_count(), 2);

            __otel_span_exit(h2);
            assert_eq!(active_span_count(), 1);

            __otel_span_exit(h1);
            assert_eq!(active_span_count(), 0);
        }
    }

    /// Unknown handle: `set_error(9999)` -> no panic, no error stamped on an unrelated span.
    ///
    /// Confirms the reverse-lookup returning `None` leaves the top span's status `Unset`.
    #[test]
    fn test_unknown_handle_set_error_leaves_top_span_unset() {
        let exporter = get_test_exporter();
        exporter.reset();

        assert_eq!(active_span_count(), 0);
        let name = b"top_span";
        let file = b"src/lib.rs";

        unsafe {
            let handle =
                __otel_span_enter(name.as_ptr(), name.len(), file.as_ptr(), file.len(), 42, 0);
            assert_ne!(handle, 0);
            assert_eq!(active_span_count(), 1);

            // Call set_error with an unknown handle (9999)
            __otel_span_set_error(9999);

            // Exit the span, causing it to finish and export to our InMemorySpanExporter
            __otel_span_exit(handle);
            assert_eq!(active_span_count(), 0);
        }

        let finished_spans = exporter.get_finished_spans().expect("get finished spans");
        let top_span = finished_spans
            .iter()
            .find(|s| s.name == "top_span")
            .expect("top_span should have exported");
        assert_eq!(
            top_span.status,
            opentelemetry::trace::Status::Unset,
            "Top span status must remain Unset after set_error(9999)"
        );

        // Verification contrast: calling set_error with the real handle DOES stamp Error
        unsafe {
            let handle_err =
                __otel_span_enter(b"err_span".as_ptr(), 8, file.as_ptr(), file.len(), 43, 0);
            __otel_span_set_error(handle_err);
            __otel_span_exit(handle_err);
        }
        let finished_spans = exporter.get_finished_spans().expect("get finished spans");
        let err_span = finished_spans
            .iter()
            .find(|s| s.name == "err_span")
            .expect("err_span should have exported");
        assert!(
            matches!(err_span.status, opentelemetry::trace::Status::Error { .. }),
            "Valid handle must have Error status stamped"
        );
    }

    /// Pointer edges on `__otel_span_enter`: null name, `name_len == 0`, and invalid UTF-8 bytes.
    ///
    /// Must return 0 without UB, and must not poison the stack.
    #[test]
    fn test_pointer_edges_on_enter() {
        assert_eq!(active_span_count(), 0);
        let valid_bytes = b"valid_func";
        let invalid_utf8 = [0xFF, 0xFE, 0xFD];
        let file = b"src/lib.rs";

        unsafe {
            // Null pointer with length 0
            assert_eq!(
                __otel_span_enter(std::ptr::null(), 0, file.as_ptr(), file.len(), 1, 0),
                0
            );
            assert_eq!(active_span_count(), 0);

            // Null pointer with length > 0
            assert_eq!(
                __otel_span_enter(std::ptr::null(), 10, file.as_ptr(), file.len(), 1, 0),
                0
            );
            assert_eq!(active_span_count(), 0);

            // Non-null pointer with length 0
            assert_eq!(
                __otel_span_enter(valid_bytes.as_ptr(), 0, file.as_ptr(), file.len(), 1, 0),
                0
            );
            assert_eq!(active_span_count(), 0);

            // Invalid UTF-8 bytes
            assert_eq!(
                __otel_span_enter(
                    invalid_utf8.as_ptr(),
                    invalid_utf8.len(),
                    file.as_ptr(),
                    file.len(),
                    1,
                    0
                ),
                0
            );
            assert_eq!(active_span_count(), 0);

            // Ensure stack is completely unpoisoned by performing a valid enter/exit cycle
            let h = __otel_span_enter(
                valid_bytes.as_ptr(),
                valid_bytes.len(),
                file.as_ptr(),
                file.len(),
                1,
                0,
            );
            assert_ne!(h, 0, "Valid span enter must succeed after edge cases");
            assert_eq!(active_span_count(), 1);

            __otel_span_exit(h);
            assert_eq!(active_span_count(), 0, "Stack must cleanly return to 0");
        }
    }

    /// Thread isolation: Handle from thread A used on thread B -> B's stack unaffected.
    ///
    /// Documents the per-thread boundary rather than leaving it implicit.
    #[test]
    fn test_thread_isolation() {
        assert_eq!(active_span_count(), 0);
        let name_a = b"span_thread_a";
        let file = b"src/lib.rs";

        unsafe {
            // Thread A enters span
            let handle_a = __otel_span_enter(
                name_a.as_ptr(),
                name_a.len(),
                file.as_ptr(),
                file.len(),
                10,
                0,
            );
            assert_ne!(handle_a, 0);
            assert_eq!(active_span_count(), 1, "Thread A has 1 active span");

            // Spawn Thread B and pass handle_a to it
            let handle = std::thread::spawn(move || {
                assert_eq!(active_span_count(), 0, "Thread B stack must start at 0");

                let name_b = b"span_thread_b";
                let handle_b = __otel_span_enter(
                    name_b.as_ptr(),
                    name_b.len(),
                    file.as_ptr(),
                    file.len(),
                    20,
                    0,
                );
                assert_ne!(handle_b, 0);
                assert_eq!(active_span_count(), 1, "Thread B has 1 active span");

                // Thread B attempts to use Thread A's handle
                __otel_span_set_error(handle_a);
                assert_eq!(
                    active_span_count(),
                    1,
                    "Thread B stack unaffected by foreign set_error"
                );

                __otel_span_exit(handle_a);
                assert_eq!(
                    active_span_count(),
                    1,
                    "Thread B stack must NOT pop handle_b when given Thread A's handle_a"
                );

                // Clean up Thread B's own span
                __otel_span_exit(handle_b);
                assert_eq!(active_span_count(), 0, "Thread B stack cleanly empty");
            });

            handle.join().expect("thread B panicked");

            // Thread A's span was unaffected by Thread B
            assert_eq!(
                active_span_count(),
                1,
                "Thread A stack must remain intact after Thread B operations"
            );

            __otel_span_exit(handle_a);
            assert_eq!(active_span_count(), 0, "Thread A stack cleanly empty");
        }
    }
}
