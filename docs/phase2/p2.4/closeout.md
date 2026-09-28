# P2.4 closeout and acceptance evidence

**Date:** 2026-09-28. **Status:** Complete within the accepted Hybrid Fallback scope.

This is the current P2.4 acceptance record. It supersedes the historical remaining-work lists and snapshots in `p2.4-validation-audit.md`. Phase 2 remains in progress because P2.5 scale/performance and broad platform certification are separate milestones.

## Public workflow (#4)

Apply first-party instrumentation with `cargo instrument-rust --apply`, review/commit those edits, then build or run with:

```text
cargo instrument --with-dependencies -- build
cargo instrument --with-dependencies -- run -- <application arguments>
```

`CARGO_INSTRUMENT_DEPENDENCIES=1` selects the same policy. No user needs to set `CARGO_INSTRUMENT_REGISTRY`. The wrapper excludes every Cargo workspace member, host/proc-macro units, telemetry packages, and the telemetry/executor dependency closure. Both unowned local path and registry dependencies are eligible. A dependency shared with the telemetry/executor implementation is conservatively excluded to prevent instrumentation recursion and preserve captured artifacts.

The first-party `cargo instrument-rust` command and Cargo's rustfix proxy ownership are unchanged. The old wrapper command without the new flag remains available for compatibility. Instrumentation policy changes in an existing target directory use supported `cargo clean` before rebuilding, so artifacts produced under one policy cannot silently survive into the other.

Native orchestration is supported for ordinary host `build` and `run`. Cross-target and all-target/test/bench commands retain the documented wrapper-only fallback. Missing/incompatible native artifacts fall back to the synchronous C ABI when a safe shim provider is proved; otherwise the original source is compiled. Async functions and spawn propagation are conservatively skipped in the C-ABI fallback. This boundary is part of the accepted Hybrid Fallback architecture, not a claim of async C-ABI support.

## Async lifecycle (#7)

Native dependency futures start their span on first poll, attach context only during each poll through `FutureExt::with_context`, and own a lifecycle guard inside the future. The guard sets the tool-specific attribute `cargo.instrumentation.async.outcome` and ends the span once:

| Exit | Outcome | OpenTelemetry status |
| --- | --- | --- |
| Normal value / early return / `Ok` | `completed` | Unset |
| `Err` / propagated `?` | `completed` | Error with empty description |
| Dropped after polling, task abort, losing select branch | `cancelled` | Unset |
| Panic with unwinding | `unwound` | Unset |
| Dropped before first poll | No span was started | No exported span |
| Process abort | Destructors do not run | Export not guaranteed |

Cancellation is not classified as success or forced into an invented error convention. No user argument/error value is read or formatted. The outcome attribute is explicitly a project convention, not an upstream semantic convention. The frozen Phase 1 first-party cancellation behavior remains documented in the research contract; the new outcome attribute applies to native dependency instrumentation.

Upstream API basis: [OpenTelemetry 0.32 SpanRef](https://docs.rs/opentelemetry/0.32.0/opentelemetry/trace/struct.SpanRef.html) exposes `set_attribute` and `end`; successful spans retain the default Unset status. The end-to-end exporter checks establish the behavior of this implementation.

## Stream and Sink decision (#8)

Do not generate spans for `Future::poll`, `Stream::poll_next`, `Sink::poll_ready`, `start_send`, `poll_flush`, or `poll_close`. These method invocations do not identify ownership of the complete operation or stream lifetime; Pending is not completion, flush is not close, and retry counts must not determine span counts. A span per item/poll would create unbounded telemetry proportional to traffic or executor scheduling.

The shared policy recognizes these method names on trait implementations and the dependency AST frontend excludes them. Qualified trait paths are handled by the final trait identifier. Trait aliases and other unresolved forms remain a syntax-front-end limit; lifetime-level stream/sink spans require a future explicit ownership API. No automatic item values, counters, or events are emitted. Ordinary async functions consuming streams/sinks keep their function lifetime span. An unrelated free/inherent method named `poll_next` remains eligible.

This is the minimal implementation justified by #8's semantics-first acceptance criteria. It does not promise automatic stream lifetime spans. Tests check all six polling boundaries and byte-identical second-pass transformation.

Upstream basis: [`Stream::poll_next`](https://docs.rs/futures-core/0.3.31/futures_core/stream/trait.Stream.html) distinguishes a yielded item from exhaustion; [`Sink`](https://docs.rs/futures/latest/futures/prelude/trait.Sink.html) separates readiness, sending, flushing, and closing. The decision to exclude these adapter boundaries is this project's inference from those contracts.

## Fallback metadata (R-1/R-2)

The shim advertises `metadata-v2` and exports `__otel_span_enter_v2`, adding crate scope to the original arguments. The public opt-in wrapper selects it only after a Cargo compiler-artifact reports that capability. Older/unknown shim providers retain the original ABI, which stays available. The current shim records function/file/line and maps the transmitted kind. Versioned emission embeds the original dependency source path and original function line instead of reporting the mirrored file and injected-line offset. Existing panic containment, null handles, LIFO cleanup, unsafe policy, and reentrancy guards remain in force.

## H3 pipelined artifact identity

Cargo pipelining may pass `--extern tokio=<rmeta>` rather than an rlib. Both paths are accepted only when Cargo reported them for the same package, target, profile, and feature set. OpenTelemetry bindings use the same companion-path proof. Metadata companions are captured from `compiler-artifact.filenames`, never guessed by changing an extension. Decoys, missing files, pathless bindings, missing records, ambiguous package/artifact identity, and missing `rt` suppress spawn rewrites. The public runtime fixture caught and now covers this ordinary CLI case. The isolated Linux migration fixture captures Tokio in its provider prepass too; package identity without an artifact record cannot authorize rewriting.

## Acceptance matrix (#9)

| Coverage | Retained executable evidence |
| --- | --- |
| Public flag and environment opt-in, forwarded app flags | `dependency_instrumentation_e2e_tests` |
| First-party source preservation and manual span coexistence | Public runtime fixture; P2.3 apply suite |
| Sync/async path dependencies and real census registry spans | Public runtime fixture |
| Spawn parenting across workers and valid parent IDs | Public runtime fixture; `tokio_spawn_tests`; R-4 migration proof |
| Completion/error/early return/abort/select/drop/unpolled/panic | Public runtime fixture |
| Source, manifest, lockfile immutability | Every public runtime build cycle |
| Cold/repeat/incremental app/incremental dependency/clean | Public runtime fixture; H1 production suite |
| `deny(warnings)` | Dependency and app fixtures; native emitter compilation proof |
| Missing provider, failed/malformed prepass, policy changes | Public fail-open fixture; H1 production suite |
| Native/Tier-2 mixture and original-source metadata | Metadata fallback fixture; H1 profile mismatch test |
| Artifact/target/profile/features/ambiguity/stale paths | H2 and H3 suites; metadata companion test |
| Polling semantics and full discovery/transformation idempotence | Poll adapter regression |
| Registry source cache immutability | Gated registry and trampoline tests |
| P2.1–P2.3 regression | Workspace suite and pinned-nightly apply round trip |

## Validation commands

Measured locally on 2026-09-28 with stable Rust 1.97.1:

| Run | Result |
| --- | --- |
| Windows fmt and workspace/all-target clippy, warnings denied | Pass |
| Windows final full workspace regression suite | 245 passed; 17 gated tests ignored |
| Windows latest public P2.4, H2, and H3 focused suites | 36 passed |
| Windows pinned-nightly P2.3 apply round trip | 1 passed |
| Windows gated registry E2E and cache immutability | 3 + 1 passed |
| Windows shim without `metadata-v2` | 10 passed |
| Linux ELF isolated R-4 migration proof | 1 passed |
| Linux ELF public P2.4 acceptance suite | 5 passed |

The Linux migration proof includes cold/repeat/incremental/clean rebuilds and
deterministic cross-thread future migration; the public suite exercises ordinary
CLI orchestration and Tokio tasks on a multithreaded runtime. Local logs are under
ignored `target/p24-*.log`; the retained reproducible evidence is the committed test
suite and CI definitions. A configured CI matrix is not itself a claim that a
remote run or broad platform certification has passed.

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo test --workspace --offline -- --test-threads=2
cargo test -p cargo-instrument --test source_instrumentation_apply_tests -- --ignored
cargo test -p cargo-instrument --test native_artifact_injection_tests -- --ignored
cargo test -p cargo-instrument --test e2e_registry_tests -- --ignored
cargo test -p cargo-instrument --test trampoline_tests test_registry_source_cache_immutability -- --ignored
```

The legacy gated registry tests require `CARGO_INSTRUMENT_REGISTRY=1`; it is a test harness compatibility setting. Offline nested test builds use `CARGO_NET_OFFLINE=true` after dependencies have been fetched. CI's existing Ubuntu/Windows/macOS workspace matrix includes the new public P2.4 tests; the isolated R-4 probe remains Linux-only. P2.5 owns large graphs, tracer caching, performance budgets, and broad platform/toolchain certification.
