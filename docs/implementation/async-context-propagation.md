# Async Context Propagation

[Project README](../../README.md) · [Tokio spawn propagation](tokio-spawn-propagation.md)

## Native emission

NativeOtelEmitter creates an internal span when an instrumented async function is first polled. Creating an unpolled future does not execute its body or create a span.

The original body runs inside `opentelemetry::trace::FutureExt::with_context(async move { ... }, span_context).await`. Context attaches during each poll and detaches when that poll returns. No thread-local context guard is held across suspension. Context follows executor task migration without adding a non-Send guard to the future.

Duration measures elapsed wall time, including suspension; busy and idle time are not separately reported.

## Results and dependency lifecycle

For recognized Result-returning async candidates, generated code awaits the body, marks Err with `Status::error("")`, and returns the original result without stringifying the error.

Dependency native emission additionally creates a drop guard that records the shared dependency async outcome attribute and ends the span:

| Outcome | Condition |
| --- | --- |
| `completed` | Normal return, including Err |
| `unwound` | Guard drops while the thread is panicking |
| `cancelled` | Started future drops before normal completion, outside unwinding |

An Err therefore has a completed lifecycle and an error span status. Unpolled futures have no lifecycle guard. Panic abort does not run drop handlers.

The ordinary native emitter does not add the dependency outcome guard. The C ABI fallback uses its own future wrapper and shim lifecycle protocol without naming OpenTelemetry types in dependency source.

## Boundaries

- [x] Body context follows polling and is detached while suspended.
- [x] Task dispatch requires separate capture; see the Tokio guide.
- [x] Explicit instrumentation and unsupported forms follow the analyzer's exclusion policy.
- [x] Stream and Sink instrumentation are outside this future-body path.

The wrapper's syntactic analyzer and the separate compiler HIR frontend have different candidate recognition boundaries. This guide describes wrapper native emission.

## Code and coverage

See [transform.rs](../../cargo-instrument/src/transform.rs), [ast.rs](../../cargo-instrument/src/ast.rs), [wrapper.rs](../../cargo-instrument/src/wrapper.rs), [shim implementation](../../otel-shim/src/lib.rs), and [shared semantics](../../instrument-semantics/src/lib.rs).

Executable coverage lives in [native OpenTelemetry tests](../../cargo-instrument/tests/native_otel_tests.rs) and [artifact injection tests](../../cargo-instrument/tests/native_artifact_injection_tests.rs). These references do not claim a new test run.
