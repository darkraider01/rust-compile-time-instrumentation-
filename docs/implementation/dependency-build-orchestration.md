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

## Transitive Native Dependency Search Paths

When an upstream dependency (such as `walkdir` or `memmap2`) is transformed to link against native OpenTelemetry, its compiled metadata (`.rmeta`) contains references to `opentelemetry` and its dependencies. Downstream units in the Cargo build graph (e.g. `ignore`, `grep-searcher`, or the final application binary) link against these transformed libraries.

Because Cargo generates `--extern` and `-L dependency` flags strictly for crates declared in each package's `Cargo.toml`, rustc would otherwise fail with `E0463: can't find crate for opentelemetry` when compiling downstream units that do not directly declare an OpenTelemetry dependency.

### Search Path Origin and Invariants

Permitted search paths originate exclusively from validated `compiler-artifact` JSON records captured during the active Cargo session plan. They are never discovered by ad-hoc filesystem traversal or recursive directory globbing (e.g. `build/*/*/out`).

The session plan validates search paths via `SessionPlan::validated_native_otel_search_paths`:

1. **Host-Unit Exclusion**: Host-only packages (build scripts, procedural macros, and their exclusive dependencies) never receive target OpenTelemetry search paths, preventing cross-compilation host/target contamination.
2. **Target Triple Match**: The unit's compilation target (`--target` or host default) must exactly match the OpenTelemetry artifact's target.
3. **Profile Agreement**: The unit's optimization and debug assertions profile must match the OpenTelemetry artifact's profile.
4. **Transitive Dependency Reachability**: Search paths are injected if and only if the unit's package transitively depends on OpenTelemetry or on an upstream package configured/eligible for native OpenTelemetry instrumentation. Unrelated crates in the workspace graph do not receive these search paths.
5. **Disk Existence & Determinism**: Every directory path is verified to exist on disk (`dir.is_dir()`), and the resulting search paths are deterministically sorted and deduplicated.

### Pre-Pass Compilation vs. Instrumentation Compilation

During `acquire_native_artifacts`, Cargo runs an initial compilation pass to capture artifact paths and assess freshness:

- **Pre-Pass Mode**: Invoked with a process-scoped `CARGO_INSTRUMENT_INTERNAL_PREPASS_TOKEN` and a verified `CARGO_INSTRUMENT_PREPASS_SESSION` pointing to the active session plan. The pre-pass wrapper does not transform source code, does not emit instrumentation stamps, and does not alter build policies. If an existing session plan from a previous build matches the current build fingerprint, the pre-pass injects validated search paths to allow units being recompiled after selective invalidation to resolve previously instrumented sibling dependencies without triggering spurious pre-pass failures.
- **Fail-Open Policy**: If the session file is missing, corrupted, or exhibits a fingerprint mismatch, the pre-pass executes plain rustc without injecting additional paths. If rustc fails due to missing dependencies, native orchestration safely disables and falls back to Tier-2 / complete package invalidation.
- **Main Instrumentation Mode**: Once artifacts are acquired and dirty packages invalidated, Cargo executes under `RUSTC_WRAPPER` where eligible units are mirrored and transformed, instrumentation/skipped stamps are written atomically, and transitive search paths are injected into dependent units.

### Current Compatibility Limitations

- Cross-compilation across multiple disparate target triples in a single Cargo invocation is not supported by native orchestration.
- Target roots with conflicting OpenTelemetry versions fail open to Tier-2 / uninstrumented compilation.
- Invocations with explicit `--tests`, `--benches`, or `--all-targets` disable native orchestration.

## Code and coverage

See [main.rs](../../cargo-instrument/src/main.rs) for orchestration, [session.rs](../../cargo-instrument/src/session.rs) for artifact compatibility, and [wrapper.rs](../../cargo-instrument/src/wrapper.rs) for emitter selection.

Coverage lives in [orchestration tests](../../cargo-instrument/tests/native_instrumentation_orchestration_tests.rs), [artifact injection tests](../../cargo-instrument/tests/native_artifact_injection_tests.rs), and [incremental scale tests](../../cargo-instrument/tests/scale_incremental_e2e_tests.rs). These references do not claim a new test run.
