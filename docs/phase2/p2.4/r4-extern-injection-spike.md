# P2.4/R-4 `--extern` Injection Spike

**Status:** Complete as an isolated architecture experiment.  
**Recommendation:** **Further investigation required** before replacing Tier-2 C ABI.

This record is the evidence for [issue #3](https://github.com/darkraider01/rust-compile-time-instrumentation-/issues/3). It does not change ADR-011 and does not remove or refactor the existing C-ABI path.

## Question

Can a dependency which does not declare `opentelemetry` compile and export the existing native OpenTelemetry instrumentation when the wrapper injects an already-resolved artifact using:

```text
--extern opentelemetry=<exact Cargo-produced rlib>
```

without mutating its manifest or source?

## Isolated implementation

The spike is intentionally test-only and requires both environment variables:

```text
CARGO_INSTRUMENT_EXPERIMENTAL_EXTERN_OTEL_CRATE=dep_r4
CARGO_INSTRUMENT_EXPERIMENTAL_EXTERN_OTEL_RLIB=<exact .rlib path>
```

The wrapper applies native `NativeOtelEmitter` output only to the named non-application crate, appends that exact `--extern` argument, and otherwise retains the normal Tier-2 C-ABI selection. There is no CLI flag, artifact globbing, or production artifact resolver in this spike.

An absent or non-rlib path logs an S11 fail-open warning and preserves the C-ABI route. This protects the experiment from selecting an arbitrary artifact and proves the C ABI remains available as a fallback.

The runnable evidence is deliberately gated because it builds an isolated multi-crate Cargo workspace:

```powershell
cargo test -p cargo-instrument --test r4_extern_injection_tests -- --ignored --nocapture
```

## Result

The test creates a path dependency, `dep_r4`, whose `Cargo.toml` has an empty `[dependencies]` table. The normal wrapper mirrors and transforms its synchronous and asynchronous functions using the established native P1.5/P1.6 emitter; only the mirror contains the OpenTelemetry calls.

The pre-pass first builds a workspace member with the same `opentelemetry` and `opentelemetry_sdk` feature set used by the application. The exact resulting `libopentelemetry-*.rlib` is supplied to the wrapper. The application installs an `InMemorySpanExporter` and proves:

- the transformed synchronous dependency span exports under the manually-created application parent;
- the transformed suspended/resumed async dependency span exports under that same parent; and
- the generated dependency mirror uses `FutureExt::with_context` and no `__otel_span_enter` C-ABI call.

### Important negative result: bare pre-pass feature mismatch

The first version of the probe prebuilt a member that declared only `opentelemetry`. The main application also resolved `opentelemetry_sdk`, which changed the resolved OpenTelemetry feature set and produced a distinct crate artifact. The dependency then compiled, but its native span used a different `global` provider and did not reach the application's exporter.

This is evidence, not an implementation detail to hide: a production pre-pass must build the **target root's feature-compatible resolved graph** and capture Cargo's authoritative artifact path. It cannot choose a package merely because it has the same name and version.

### `tokio::spawn` observation

The probe separately invokes:

```rust
tokio::spawn(async { dep_r4::async_work().await }).await
```

on a multi-thread Tokio runtime. The spawned dependency span has `SpanId::INVALID` as its parent rather than the enclosing application parent. Native `FutureExt::with_context` preserves context across suspension and resumption of the instrumented future; it does not itself capture context at `tokio::spawn` task creation.

This is the expected boundary for #6. The spike does not introduce a synthetic task span or claim automatic spawn propagation.

## Acceptance evidence

| Criterion | Result |
| --- | --- |
| Path dependency fixture | Pass: isolated `dep_r4` workspace member. |
| Dependency declares no `opentelemetry` | Pass: empty dependency table is asserted. |
| Native OTel compiles via injected `--extern` | Pass: the dependency mirror compiles with the exact prebuilt rlib. |
| Synchronous dependency span exports | Pass: `sync_work` is a child of `r4_parent`. |
| P1.6 async inside dependency | Pass: `async_work` suspends once and resumes under native `FutureExt::with_context`. |
| Parent/child relationship | Pass for direct sync and async dependency calls. |
| `tokio::spawn` separately tested | Pass as an observation: parent context is not propagated automatically. |
| Dependency source and manifest immutable | Pass: byte snapshots before/after. |
| Registry cache untouched | Pass: the cached `opentelemetry-0.32.0` source tree is byte-snapshotted before/after. |
| Cold, repeat, incremental, clean rebuild | Pass: the gated probe executes all four. The clean rebuild reruns the artifact pre-pass. |
| Missing artifact fails open | Pass: the wrapper warns and retains existing Tier-2 C ABI; a subsequent `cargo check` succeeds. |
| Ambiguous artifacts never arbitrarily selected | Pass for the injection mechanism: a second plausible `libopentelemetry-*.rlib` filename is present, but the wrapper injects only its explicit path. Genuine multi-version graph resolution remains open below. |
| Host/proc-macro units excluded | Pass: the fixture includes a proc-macro crate; it is not entered into the eligible analysis/injection path. |
| C ABI retained | Pass: no C-ABI code was removed; missing-artifact fallback exercises its existing selection path. |

## Remaining blockers

1. The production `SessionPlan` has no per-target-root, feature-compatible pre-pass or JSON artifact capture. Implementing one is follow-on work, not part of this spike.
2. A genuine two-version graph still needs the planned version-qualified `cargo --message-format=json` artifact selection test. The explicit-path mechanism is safe; package/feature selection is not yet automated.
3. A dependency shared by target roots that resolve incompatible OpenTelemetry versions remains structurally unsatisfiable for a single native-instrumented compilation unit.
4. Automatic Tokio task-boundary propagation needs the distinct #6 design and implementation.
5. Cross-target and proc-macro/build-script coverage has only the narrow invocation-exclusion proof here; broader P2.5 validation remains out of scope.

## Conclusion

**4. Further investigation required.**

`--extern` injection is viable for a dependency when the wrapper receives an authoritative artifact that matches the target root's resolved OpenTelemetry feature set. It is not yet safe to replace Tier-2 C ABI because the project has not implemented a deterministic, per-target-root artifact pre-pass/selection mechanism and has not resolved the shared-dependency multi-version limit. Keep the C ABI intact while that work is evaluated.
