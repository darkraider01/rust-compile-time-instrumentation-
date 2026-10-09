# Phase 3 — Repeat-Build Metadata and Pre-Pass Reuse Design Investigation

[Phase 3 Roadmap](phase-3-evaluation.md) | [Repeat-Build Profiling Note](phase-3-repeat-build-profiling-2026-10-09.md) | [Steady-State Overhead Note](phase-3-overhead-pilot-2026-10-08-steady-state.md)

**Date**: 2026-10-09  
**Status**: Completed Design Investigation (Investigation Only; No Production Code Changes Implemented)  
**Objective**: Investigate whether repeat-build `cargo metadata` (~57 ms) or artifact pre-pass work (~66 ms) can be safely reused, map the invalidation surface, demonstrate failure modes via empirical experiments, establish a provisional invalidation inventory, and evaluate trade-offs before recommending an implementation path.

---

## 1. Executive Summary

Phase 3 repeat-build profiling established that **98.2% of internal CLI wall time** on repeat builds (~193.3 ms out of ~196.8 ms) is spent executing three sequential Cargo subprocesses:
1. `cargo metadata`: **~57.0 ms** (28.9%)
2. Unwrapped pre-pass build (`cargo build --message-format=json-render-diagnostics`): **~65.9 ms** (33.5%)
3. Wrapped build (`cargo build` with `RUSTC_WRAPPER`): **~70.3 ms** (35.7%)
4. Internal in-process CLI orchestration: **~3.93 ms** (2.0%)

This investigation examined whether the two preparatory stages (`cargo metadata` and the unwrapped pre-pass build) can be safely skipped or cached on repeat/no-op builds.

### Key Conclusions:

1. **Manifest-Only Validation is Insufficient for Pre-Pass Reuse**:
   A source code edit demonstrates why manifest-only validation is insufficient to justify skipping the pre-pass. Cargo metadata and manifest hashes depend on manifests (`Cargo.toml`), the lockfile (`Cargo.lock`), configuration, and filesystem target discovery; they **do not inspect `.rs` source code contents**. If a developer edits a source file in a dependency or workspace crate, manifests and metadata output remain 100% identical, while the compiled `.rlib` artifact becomes dirty. Skipping the pre-pass based on manifest or metadata checks would bypass Cargo's dependency compilation and artifact stamp verification (`uninstrumented_fresh_package_ids`), causing the wrapped build to encounter dirty dependencies without captured native artifact coordinates. While this does not prove that every conceivable pre-pass reuse architecture (such as one integrating directly with Cargo's artifact query engine) is impossible, it establishes that simple manifest- or metadata-based pre-pass omission without Cargo verification is unsafe.

2. **Metadata Caching Savings Have a Strict Ceiling and High Invalidation Complexity**:
   Reusing `cargo metadata` output or caching `SessionPlan` across CLI invocations has a theoretical savings ceiling of ~57 ms (the duration of the metadata subprocess before accounting for fingerprinting overhead). Even if the entire metadata phase were eliminated with zero hash overhead, repeat builds would remain at ~140 ms, which **would not achieve a sub-100 ms repeat-build goal**.
   Furthermore:
   - A naive cache (e.g. hashing only workspace `Cargo.toml` and `Cargo.lock`) produces false hits when external path dependencies change, when CLI feature flags vary, when `.cargo/config.toml` changes, or when targets are dynamically discovered from filesystem layout.
   - `build_session_plan()` currently returns `(SessionPlan, serde_json::Value)`. Downstream artifact acquisition (`acquire_native_artifacts()`) directly inspects the raw `serde_json::Value` to resolve package versions, package sources (null vs. registry), and dependency graph edges for the retained OpenTelemetry closure.
   - Decoupling `acquire_native_artifacts()` requires expanding `SessionPlan` to persist these structured graph fields.

3. **Core Conclusion & Recommendation**:
   **Manifest-only reuse is insufficient; no complete reuse contract has yet been established.**
   - **Retain the Status Quo (Option 1) for the current Phase 3 evaluation**: Querying Cargo on every invocation avoids maintaining an ad-hoc cache invalidation engine that mimics Cargo's configuration and target discovery rules, and preserves all fail-open invariants.
   - **Deferred Investigation (Option 2)**: If sub-100ms repeat builds are mandated in a future phase, it will require optimizing both metadata and artifact acquisition in tandem. If metadata-only caching is pursued, it must follow the provisional invalidation inventory with a strict fail-open contract while keeping the unwrapped pre-pass build intact.

---

## 2. Code Contracts and Data Flow Analysis

### 2.1 The Current Orchestration Flow
In `cargo-instrument/src/main.rs` (`execute_cargo_with_wrapper`):
```text
┌────────────────────────────────────────────────────────────────────────┐
│ cargo-instrument CLI Invocation                                        │
└────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
       1. build_session_plan(&invocation.cargo_args, &current_dir)
          └── Executes: `cargo metadata --format-version 1 ...` (~57.0 ms)
          └── Returns: (SessionPlan, serde_json::Value)
                                    │
                                    ▼
       2. acquire_native_artifacts(&args, &target_dir, &current_dir, &mut plan, &metadata)
          └── Executes: `cargo build --message-format=json-render-diagnostics` (~65.9 ms)
          └── Parses compiler-artifact JSON stream
          └── Computes desired_instrumented_package_ids(plan, metadata)
          └── Computes retained_artifact_closure(plan, metadata)
          └── Verifies stamps via uninstrumented_fresh_package_ids(...)
          └── Selectively cleans dirty instrumented units via invalidate_packages(...)
                                    │
                                    ▼
       3. plan.save_to_file(&session_file)
          └── Writes `target/instrumented/cargo_instrument_session.json`
                                    │
                                    ▼
       4. Spawns: `cargo build` with RUSTC_WRAPPER set (~70.3 ms)
          └── Child wrapper processes load SessionPlan via CARGO_INSTRUMENT_SESSION_ID
```

### 2.2 Contract and Consumers of Raw `serde_json::Value`
`build_session_plan()` returns `(SessionPlan, serde_json::Value)` where `metadata` is the raw JSON parsed from `cargo metadata`.

A code audit of `cargo-instrument/src/main.rs` reveals that `metadata` is consumed in five distinct routines:

| Consumer Routine | Location | Exact Fields Inspected in Raw `metadata` | Purpose |
|---|---|---|---|
| `plan.add_r4_artifacts_from_cargo_json` | `session.rs:275` | `packages[].id`, `packages[].version` | Associates exact package version with native OpenTelemetry `.rlib` artifacts |
| `plan.add_tokio_artifacts_from_cargo_json` | `session.rs:415` | `packages[].id`, `packages[].version`, `packages[].name` | Associates Tokio package version and discovers Tokio package IDs by name |
| `desired_instrumented_package_ids` | `main.rs:940` | `packages[].id`, `packages[].name`, `packages[].source` | Identifies candidate packages: requires `source.is_null()` (local path / member) unless `CARGO_INSTRUMENT_REGISTRY=1` is active |
| `retained_artifact_closure` | `main.rs:972` | `resolve.nodes[].id`, `resolve.nodes[].deps[].pkg` | Builds dependency adjacency graph to compute the transitive dependency closure of retained native OpenTelemetry artifacts |
| `invalidate_packages` | `main.rs:1026` | `packages[].id`, `packages[].name` | Builds `name -> Vec<id>` map to prevent unsafe selective cleaning when multiple package versions share the same name, or when a package shares a name with the retained closure |

### 2.3 Candidate Decoupling of `acquire_native_artifacts()` from Raw JSON
`SessionPlan` already stores:
- `package_names_by_id: HashMap<String, String>`
- `package_manifest_dirs: HashMap<String, PathBuf>`
- `workspace_package_ids: HashSet<String>`
- `target_reachable_package_ids: HashSet<String>`
- `wrapper_excluded_package_ids: HashSet<String>`
- `package_dependencies: HashMap<String, Vec<CargoDepEdge>>`

A candidate design to decouple `acquire_native_artifacts()` from `serde_json::Value` would map the remaining metadata accesses to structured fields:
1. `package_versions_by_id: HashMap<String, String>` (for artifact version registration)
2. `package_sources_by_id: HashMap<String, Option<String>>` (to differentiate local path dependencies from registry crates)
3. `package_ids_by_name: HashMap<String, Vec<String>>` (to detect name collisions and multi-version packages during selective cleaning)

However, presenting this three-field mapping is an architectural candidate, not proof that decoupling requires only these fields. Full semantic parity with Cargo's resolver and the strict separation of metadata-derived initial state from acquisition-mutated session state (such as registered artifacts, shims, and closures) would require empirical verification before any refactoring could be justified. Artifact acquisition has not been refactored and these fields have not been added.

---

## 3. Invalidation Surface Inventory (Provisional)

To avoid conflating stages, the invalidation inputs are separated into **metadata-resolution inputs** (inputs that alter the graph and package definitions returned by `cargo metadata`) and **artifact-build/policy inputs** (inputs that govern compilation profiles, triples, and runtime behavior).

```text
┌────────────────────────────────────────────────────────────────────────┐
│ Provisional Invalidation Surface Inventory                             │
├────────────────────────────────┬───────────────────────────────────────┤
│ A. Metadata-Resolution Inputs  │                                       │
│ 1. Manifests & Dependencies    │ • Workspace root Cargo.toml           │
│                                │ • Workspace Cargo.lock                │
│                                │ • Member Cargo.toml files             │
│                                │ • External path-dep Cargo.toml files  │
│ 2. Forwarded CLI Flags         │ • --features <list>                   │
│    (in cargo_metadata_output)  │ • --no-default-features               │
│                                │ • --all-features                      │
│                                │ • --config <key=value>                │
│                                │ • --manifest-path <path>              │
│                                │ • --filter-platform <triple>          │
│                                │ • --offline, --locked, --frozen       │
│ 3. Cargo Configuration Files   │ • <workspace>/.cargo/config.toml      │
│                                │ • <workspace>/.cargo/config (legacy)  │
│                                │ • Parent directory .cargo/config*     │
│                                │ • $CARGO_HOME/config.toml and config  │
│ 4. Target File Discovery       │ • src/lib.rs                          │
│                                │ • src/main.rs                         │
│                                │ • src/bin/*.rs and src/bin/*/main.rs  │
│                                │ • tests/*.rs and tests/*/main.rs      │
│                                │ • benches/*.rs and benches/*/main.rs  │
│                                │ • examples/*.rs and examples/*/main.rs│
│                                │ • build.rs                            │
├────────────────────────────────┼───────────────────────────────────────┤
│ B. Artifact-Build & Policy     │                                       │
│ 1. Compilation Profile         │ • --release (profile selection)       │
│ 2. Target Triple               │ • --target <triple>                   │
│ 3. Compiler Environment        │ • RUSTFLAGS, CARGO_ENCODED_RUSTFLAGS  │
│                                │ • CARGO_TARGET_DIR, CARGO_BUILD_TARGET│
│ 4. Package Selection           │ • -p / --package (in-memory scoping)  │
│ 5. Telemetry Policy Flags      │ • CARGO_INSTRUMENT_DEPENDENCIES       │
│                                │ • CARGO_INSTRUMENT_REGISTRY           │
│ 6. Toolchain Identity          │ • rustc -vV and cargo -vV             │
└────────────────────────────────┴───────────────────────────────────────┘
```

Notice that `cargo-instrument`'s metadata helper (`cargo_metadata_output` in `main.rs:773-797`) does **not** forward `--target` or `--release` to `cargo metadata`; package selection (`-p / --package`) is resolved in memory *after* querying full workspace metadata.

---

## 4. Empirical Invalidation Experiments and Failure Modes

Four deterministic empirical test harnesses were executed and verified via programmatic assertions (preserved in `evidence/phase3-repeat-build-reuse-investigation-2026-10-09/run-experiments.py`).

### Experiment A: External Path Dependency Manifest Mutation
* **Setup**: Workspace with root `Cargo.toml` and member `app`. `app` depends on `external_dep = { path = "../../external_dep" }` outside the workspace root.
* **Mutation**: Added a new feature flag (`newly_added_feature = []`) to `external_dep/Cargo.toml`. Workspace manifests and `Cargo.lock` remained untouched.
* **Observed & Asserted Results**:
  - Workspace `Cargo.toml`, `Cargo.lock`, and member `app/Cargo.toml` SHA-256 hashes remained **100% identical**.
  - `cargo metadata` output diverged: `meta_before != meta_after`.
  - Resolved features for `external_dep` gained `'newly_added_feature'` (asserted present in `meta_after`, absent in `meta_before`).
* **Conclusion**: Manifest-only checking of workspace members produces false cache hits when external path dependencies change.

### Experiment B: Metadata-Resolution Flags vs. Compiler Environment
* **Setup**: Package with features `default = ["std"]`, `std = []`, and `extra = []`.
* **Queries**:
  1. Default invocation (`cargo metadata`).
  2. With forwarded flag `--features extra`.
  3. With forwarded flag `--no-default-features`.
  4. With compiler environment override `RUSTFLAGS="--cfg custom_cfg"`.
* **Observed & Asserted Results**:
  - Default resolved features: `{'default', 'std'}`.
  - `--features extra` resolved features: `{'default', 'std', 'extra'}` (`meta_default != meta_feat`).
  - `--no-default-features` resolved features: `set()` (`meta_default != meta_no_def`).
  - `RUSTFLAGS` in the environment produced **100% identical metadata** (`meta_default == meta_rustflags`).
* **Conclusion**: Metadata-resolution flags (`--features`, `--no-default-features`) alter metadata resolve nodes without filesystem changes. Compiler environment variables (`RUSTFLAGS`) govern artifact compilation, not metadata resolution.

### Experiment C: Dynamic Target Discovery Across File Conventions
* **Setup**: Package with initial `src/lib.rs` and single target `[('pkg', ['lib'])]`. `Cargo.toml` was hashed.
* **Mutation**: Added files covering Cargo target discovery conventions without modifying `Cargo.toml`:
  - `src/main.rs` (main binary)
  - `src/bin/tool.rs` (named binary)
  - `src/bin/daemon/main.rs` (nested named binary)
  - `tests/integ.rs` (integration test)
  - `tests/suite/main.rs` (nested integration test)
  - `benches/bench1.rs` (benchmark)
  - `examples/ex1.rs` (example)
  - `build.rs` (custom build script)
* **Observed & Asserted Results**:
  - `Cargo.toml` SHA-256 remained **100% identical**.
  - Discovered targets expanded from 1 to 9 distinct targets (asserted all 8 added targets present in `meta_after["packages"][0]["targets"]`).
* **Conclusion**: Target discovery dynamically adds targets based on filesystem layout, including nested directories and build scripts, without manifest modifications.

### Experiment D: Source Edits vs. Metadata Immutability & Pre-Pass Rebuild
* **Setup**: Two-crate workspace (`app` depending on `helper`). Initial pre-pass build executed; helper `.rlib` path and mtime captured. Initial metadata JSON captured (`meta_before`).
* **Mutation**: Modified `helper/src/lib.rs` (changed return value from 10 to 9999). Manifests and lockfile untouched.
* **Observed & Asserted Results**:
  - `cargo metadata` output before and after the edit was **100% identical** (`meta_before == meta_after` byte-for-byte; asserted equal).
  - Manifest and lockfile hashes remained **100% identical**.
  - Re-running the pre-pass `cargo build` reported helper as `fresh: false` (recompiled).
  - Helper `.rlib` mtime advanced upon recompilation (`mtime_after > mtime_before`).
* **Conclusion**: Manifest and metadata checks are completely blind to source code modifications. Manifest-only validation is insufficient to determine whether pre-pass artifacts need recompilation.

---

## 5. Provisional Invalidation Inventory and Cache Contract

This inventory represents a **provisional checklist** of identified inputs, **not an implementation-ready or proven-complete correctness contract**. Establishing closed-form completeness for Cargo's invalidation space remains an open problem.

### 5.1 Provisional Inventory Predicate
A metadata cache hit requires at minimum that all identified resolution inputs match:

$$\text{Hit}_{\text{metadata}} \iff \mathcal{H}_{\text{manifests}} = \mathcal{H}^*_{\text{manifests}} \land \mathcal{H}_{\text{configs}} = \mathcal{H}^*_{\text{configs}} \land \mathcal{H}_{\text{meta\_flags}} = \mathcal{H}^*_{\text{meta\_flags}} \land \mathcal{H}_{\text{targets}} = \mathcal{H}^*_{\text{targets}} \land \mathcal{T}_{\text{toolchain}} = \mathcal{T}^*_{\text{toolchain}}$$

Where:
1. $\mathcal{H}_{\text{manifests}} = \text{SHA256}(\text{Cargo.lock} \parallel \text{Root Cargo.toml} \parallel \sum_{p \in \text{Members} \cup \text{PathDeps}} \text{Cargo.toml}_p)$  
   *(Requires tracking the full recursive closure of external path dependencies).*
2. $\mathcal{H}_{\text{configs}} = \text{SHA256}(\sum_{c \in \text{ConfigHierarchy}} \text{File}(c))$  
   *(All `.cargo/config.toml` AND legacy `.cargo/config` files from working directory up to `/`, plus `$CARGO_HOME/config.toml` and `$CARGO_HOME/config`).*
3. $\mathcal{H}_{\text{meta\_flags}} = \text{SHA256}(\text{ForwardedFlags})$  
   *(`--features`, `--no-default-features`, `--all-features`, `--config`, `--manifest-path`, `--filter-platform`, `--offline`, `--locked`, `--frozen`).*
4. $\mathcal{H}_{\text{targets}} = \text{SHA256}(\sum_{p \in \text{Packages}} \text{TargetFiles}(p))$  
   *(Presence and names of `src/lib.rs`, `src/main.rs`, `src/bin/*.rs`, `src/bin/*/main.rs`, `tests/*.rs`, `tests/*/main.rs`, `benches/*.rs`, `benches/*/main.rs`, `examples/*.rs`, `examples/*/main.rs`, `build.rs`).*
5. $\mathcal{T}_{\text{toolchain}} = \text{SHA256}(\text{rustc -vV} \parallel \text{cargo -vV})$

### 5.2 Proposed Cache State Storage and Fail-Open Contract (Candidate Proposal)
* **Storage Location (Proposed)**: `target/instrumented/cargo_instrument_session.json` (inside the isolated target directory specified by `--target-dir`).
* **Cleanup Behavior (Qualified)**: Cache removal depends on cleaning the specific target directory in use. Ordinary `cargo clean` removes only the default `target/` directory; it does **not** remove artifacts in custom or isolated target directories such as `target/instrumented/` unless explicitly directed via `cargo clean --target-dir target/instrumented`. Automatic cache eviction therefore requires clean invocations to match the active instrumentation target directory.
* **Fail-Open Policy (Proposed Contract)**:
  Under this candidate proposal, if a cached session file were missing, stale, unreadable, invalid JSON, or failed schema validation:
  1. The cache miss would be logged when `INSTRUMENT_DEBUG=1`.
  2. The invalid cache file would be discarded.
  3. The driver would fall back immediately to running `cargo metadata` (S11 fail-open).
  4. Execution would never terminate with a user-facing error due to cache read or parse failure.
  *(Note: This is an architectural proposal for a future caching design; no cache storage or miss fallback is implemented in the current driver).*

---

## 6. Strategic Evaluation of Reuse Options

| Option | Architecture | Theoretical Savings Ceiling | Safety & Correctness Assessment | Maintenance Considerations |
|---|---|---|---|---|
| **Option 1: Status Quo (No Caching)** | Query `cargo metadata` and run unwrapped pre-pass on every command | **0 ms** (current ~197 ms repeat build) | Authoritative query on every invocation (does not rely on cached state or file hashing); zero stale state | **Zero maintenance**; no cache invalidation logic |
| **Option 2: Metadata / SessionPlan Fingerprinting** | Cache `SessionPlan` using provisional invalidation inventory; keep pre-pass build | **~57 ms ceiling** (before accounting for hashing overhead; repeat build remains ~140 ms+) | Feasible with complete inventory; fails open on miss | Must track Cargo config hierarchy, legacy files, and target discovery rules |
| **Option 3: Pre-Pass Build Skipping** | Skip unwrapped `cargo build` on repeat builds based on manifest/metadata check | **~66 ms ceiling** (repeat build remains ~131 ms+) | **Unsound under manifest-only validation** (Hazard D.2: blind to `.rs` source code edits) | Would require reimplementing file mtime dependency tracking outside Cargo |
| **Option 4: Combined Metadata + Pre-pass Skip** | Skip both metadata and pre-pass | **~123 ms ceiling** (repeat build remains ~74 ms+) | **Unsound** (inherits all critical flaws of Option 3) | Severe maintenance burden and correctness risk |

*(Note: Metadata-phase durations represent theoretical savings ceilings before fingerprinting and file-hashing overhead. Even eliminating the entire metadata phase (~57 ms) would leave repeat-build time at ~140 ms, which does not achieve a sub-100 ms repeat-build goal).*

---

## 7. Architectural Recommendation

### Primary Recommendation: Retain Option 1 (Status Quo) for Current Milestone

1. **Safety and Fail-Open Compliance**:
   Phase 3 is an evaluation and research phase. Correctness, trace fidelity, deterministic testing, and fail-open behavior are the top priorities. The current architecture ensures that every build receives authoritative metadata from Cargo and verifies all artifact stamps.

2. **Manifest-Only Reuse is Insufficient; No Complete Contract Established**:
   The empirical evidence confirms that manifest-only reuse cannot detect source code changes in pre-pass artifacts (Experiment D) or external path dependency manifest changes (Experiment A). No implementation-ready, closed-form invalidation contract currently exists.

3. **Performance Reality Check**:
   Skipping metadata alone yields at most ~57 ms savings before validation costs, leaving repeat builds above 140 ms. Implementing Option 2 would add significant complexity (traversing `.cargo/config` hierarchies, tracking path dependencies, and discovering targets) without achieving a sub-100 ms target.

---

## 8. Evidence Location, Rerun Procedure, and Checksums

Durable evidence, the automated test harness, and execution logs are preserved in [`evidence/phase3-repeat-build-reuse-investigation-2026-10-09/`](../../evidence/phase3-repeat-build-reuse-investigation-2026-10-09/):

| File | SHA-256 Checksum | Description |
|---|---|---|
| `env-manifest.txt` | `2f1f4ef55332898db81874263702063e735751a557a3ea38286416d3f4f5bd9d` | Host environment, CPU model (`lscpu`), memory, toolchain versions, lockfile identity |
| `run-experiments.py` | `746a758bc2bff0536c18cff44567028d4b0d5b3d534089e5431a6ceb54ab7af5` | Deterministic Python test harness executing Experiments A, B, C, D with programmatic assertions |
| `experiments.log` | `100655f76e070812b9437da1e80fe275f5b640a3b535bf9a12ed18ac13d0d6c8` | Complete raw output from execution of `run-experiments.py` demonstrating all passes |

### Rerun Procedure

To execute the invalidation experiments suite and verify all assertions:
```sh
# Execute deterministic invalidation experiments:
python3 ./evidence/phase3-repeat-build-reuse-investigation-2026-10-09/run-experiments.py
```
