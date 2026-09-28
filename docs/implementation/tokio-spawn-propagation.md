# Tokio Spawn Context Propagation

[Project README](../../README.md) · [Async context](async-context-propagation.md)

## Transformation

Native emission wraps the spawn argument with `opentelemetry::trace::FutureExt::with_context(future, opentelemetry::Context::current())`. The caller's context is captured during argument evaluation and attached during each poll of the dispatched future. No synthetic task span is created.

## Recognition

Recognized paths are `tokio::spawn`, `tokio::task::spawn`, and their leading-`::` forms, each with exactly one argument. Bare imported spawn, task aliases, renamed Tokio paths, spawn_blocking, and other runtimes are excluded.

A local binding named tokio conservatively excludes the source file. Idempotence requires the exact top-level `opentelemetry::trace::FutureExt::with_context` argument form; unrelated functions or methods with that name do not establish instrumentation.

## Cargo validation

Syntax alone is insufficient because an unrelated package can be renamed to tokio. Wrapper rewriting requires:

- [x] An active rustc extern binding named tokio.
- [x] An exact owning Cargo package and dependency edge with that binding.
- [x] A resolved dependency package actually named tokio.
- [x] A compatible Cargo-reported artifact with matching target/profile, runtime features, and canonical path identity.

Cargo-reported rmeta companions are supported alongside rlib paths for compiler pipelining. Missing, ambiguous, foreign, or incompatible evidence suppresses rewriting. Package metadata alone cannot replace artifact validation.

## Edit and emitter scope

Two insertions wrap the original argument. Function and spawn edits share byte-range and UTF-8 validation. Files containing only spawn sites are also mirrored.

Only NativeOtelEmitter handles spawn propagation. Trampoline and sentinel emitters do not inject this wrapper, so fallback dependency instrumentation does not automatically propagate context through Tokio spawn calls.

## Code and coverage

See [ast.rs](../../cargo-instrument/src/ast.rs), [transform.rs](../../cargo-instrument/src/transform.rs), [wrapper.rs](../../cargo-instrument/src/wrapper.rs), and [session.rs](../../cargo-instrument/src/session.rs).

Coverage lives in [spawn tests](../../cargo-instrument/tests/tokio_spawn_tests.rs) and [artifact injection tests](../../cargo-instrument/tests/native_artifact_injection_tests.rs). These references do not claim a new test run.
