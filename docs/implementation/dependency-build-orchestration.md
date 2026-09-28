# Dependency Build Orchestration

[Project README](../../README.md) · [Decision records](../decisions/adr-007-013.md)

## Usage

```sh
cargo instrument --with-dependencies -- build
cargo instrument --with-dependencies -- run
```

The wrapper rewrites compiler inputs in a source mirror and preserves original dependency source and manifests. First-party source application uses the separate `cargo instrument-rust --apply` command.

Native acquisition supports ordinary build/run invocations. Explicit `--target`, `--all-targets`, `--tests`, and `--benches` disable this orchestration path.

## Build sequence

1. Resolve Cargo metadata and the selected package graph into a session plan.
2. Run an unwrapped pre-pass in the instrumented target directory with `--message-format=json-render-diagnostics`. For run, use build during the pre-pass and execute the application only in the final invocation.
3. Capture compatible OpenTelemetry and Tokio artifacts and the shim's metadata-v2 capability.
4. Derive the desired instrumentation set from metadata, independently of artifact output.
5. Retain the native runtime artifact closure. Rebuild desired packages compiled during the pre-pass and fresh packages without a valid instrumentation marker. Already instrumented fresh packages can remain cached.
6. Selectively clean packages outside the retained closure, then verify retained OpenTelemetry libraries still exist.
7. Run the requested Cargo command through the wrapper.

Cleaning is batched in groups of up to 50 packages, with package identity and retained-closure checks. Build policy changes also participate in cache invalidation.

## Recovery

- [x] Failed or malformed output clears native OpenTelemetry and Tokio records, resets shim metadata capability, and invalidates the complete desired instrumentation set.
- [x] Desired-set overlap with the retained runtime closure abandons native acquisition and invalidates the desired set.
- [x] No eligible native artifact triggers fallback after invalidating dirty desired packages.
- [x] A retained OpenTelemetry library disappearing after cleaning triggers full desired-set invalidation and fallback.
- [x] Cleaning errors terminate with a diagnostic.

The wrapper prefers compatible native injection. Otherwise it uses the provisional C ABI when its provider is available; units without a safe route compile without rewriting. See [ADR-011](../decisions/adr-007-013.md#adr-011---the-tier-2-c-abi-is-provisional).

## Code and coverage

See [main.rs](../../cargo-instrument/src/main.rs) for orchestration, [session.rs](../../cargo-instrument/src/session.rs) for artifact compatibility, and [wrapper.rs](../../cargo-instrument/src/wrapper.rs) for emitter selection.

Coverage lives in [orchestration tests](../../cargo-instrument/tests/native_instrumentation_orchestration_tests.rs), [artifact injection tests](../../cargo-instrument/tests/native_artifact_injection_tests.rs), and [incremental scale tests](../../cargo-instrument/tests/scale_incremental_e2e_tests.rs). These references do not claim a new test run.
