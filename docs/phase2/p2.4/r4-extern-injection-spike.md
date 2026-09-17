# P2.4/R-4 `--extern` Injection Spike

**Status:** Production-oriented artifact-resolution spike complete.
**Recommendation:** **Use a hybrid fallback**; native injection is safe only for an explicitly resolved compatible unit.

This record is the evidence for [issue #3](https://github.com/darkraider01/rust-compile-time-instrumentation-/issues/3). [ADR-011](../../phase2/decision-records.md#adr-011---the-tier-2-c-abi-is-provisional) has been updated to incorporate this final Hybrid Fallback decision; the existing C-ABI path remains intact as the required fallback.

## Question

Can a dependency which does not declare `opentelemetry` compile and export the existing native OpenTelemetry instrumentation when the wrapper injects an already-resolved artifact using:

```text
--extern opentelemetry=<exact Cargo-produced rlib>
```

without mutating its manifest or source?

## Deterministic artifact resolver

The spike no longer accepts a manually supplied rlib path. `SessionPlan` now records:

- the exact dependency package ID, inferred from its manifest directory;
- the sole OpenTelemetry package ID shared by every target root that reaches that dependency;
- Cargo metadata's resolved feature set and package version; and
- exactly one `rlib` path from Cargo's `compiler-artifact` JSON output, keyed by target and profile.

The resolver rejects missing artifacts, duplicate matching artifacts, target mismatch, profile mismatch, a shared dependency with multiple OpenTelemetry package IDs, and stale paths. Every rejection preserves the existing Tier-2 C-ABI route under S11. It never scans a dependency directory or selects an artifact by filename.

The JSON pre-pass remains deliberately test-spike orchestration: it is not exposed as a user-facing CLI and is not a decision to migrate all dependency instrumentation.

The runnable evidence is deliberately gated because it builds an isolated multi-crate Cargo workspace:

```powershell
cargo test -p cargo-instrument --test r4_extern_injection_tests -- --ignored --nocapture
```

## Result

The test creates a path dependency, `dep_r4`, whose `Cargo.toml` has an empty `[dependencies]` table. The normal wrapper mirrors and transforms its synchronous and asynchronous functions using the established native P1.5/P1.6 emitter; only the mirror contains the OpenTelemetry calls.

The pre-pass first builds a workspace member with the same `opentelemetry` and `opentelemetry_sdk` feature set used by the application. Cargo emits the artifact JSON; the session-plan resolver records the exact path and the wrapper obtains it only through that plan. The application installs an `InMemorySpanExporter` and proves:

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
| Ambiguous artifacts never arbitrarily selected | Pass: the resolver stores Cargo's exact JSON artifact; a second plausible filename is ignored. Duplicate matching JSON records are rejected. |
| Multiple OTel versions / target roots | Pass: metadata fixture models 0.30 and 0.32 under two roots sharing a dependency; that dependency receives no native mapping and fails open. Root-specific packages retain their exact package ID. |
| Target/profile identity | Pass: resolver rejects host-vs-target and dev-vs-release mismatches. |
| Host/proc-macro units excluded | Pass: the fixture includes a proc-macro crate; it is not entered into the eligible analysis/injection path. |
| C ABI retained | Pass: no C-ABI code was removed; missing-artifact fallback exercises its existing selection path. |

## Remaining blockers

1. The test pre-pass is not yet integrated into a supported user-facing dependency mode. Cargo scheduling must be preserved while producing a feature-compatible artifact for the selected target root.
2. A dependency shared by target roots that resolve incompatible OpenTelemetry versions remains structurally unsatisfiable for one native-instrumented compilation unit. The resolver correctly fails open, so C ABI remains the available fallback.
3. Automatic Tokio task-boundary propagation is intentionally untouched and belongs to #6.
4. Cross-target and proc-macro/build-script coverage has only target/profile and invocation-exclusion fixtures; broader platform validation remains P2.5 work.

## Conclusion

**3. Use a hybrid.**

`--extern` injection is viable and selection-safe when `SessionPlan` has a Cargo-authoritative artifact matching the dependency's package identity, resolved features, target, and profile. It cannot safely replace Tier-2 outright: the shared multi-root/multi-version case must fail open, and the C ABI remains necessary while supported pre-pass orchestration is completed. Keep the C ABI intact.
