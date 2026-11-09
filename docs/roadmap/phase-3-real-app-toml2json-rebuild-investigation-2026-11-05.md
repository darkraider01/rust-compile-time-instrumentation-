# Phase 3 — `toml2json` Repeat-Build Invalidation Investigation (2026-11-05)

[Project README](../../README.md) · [Phase 3 roadmap](phase-3-evaluation.md) · [Real-application evaluation (2026-10-26)](phase-3-real-app-toml2json-2026-10-26.md) · [Repeat-build profiling (2026-10-09)](phase-3-repeat-build-profiling-2026-10-09.md) · [Reuse investigation (2026-10-09)](phase-3-repeat-build-reuse-investigation-2026-10-09.md)

**Date**: 2026-11-05  
**Status**: Completed Bounded Investigation & Defect Fix; Architectural Design Decision Identified for Direction  
**Objective**: Investigate and establish the causal chain for the unexpectedly slow instrumented no-change rebuild of `toml2json` (17.5 s observed vs. 0.08 s baseline), determine the invalidation set, verify certification and stamp behavior, fix unambiguous prefix-matching stamp removal defects, and present options for cache certification of skipped dependencies.

---

## 1. Baseline and Empirical Reproduction

### 1.1 Expected vs. Observed Behavior
- **Expected no-change behavior**: In the October 9 single-dependency benchmark fixture (`bench_app` + `bench_dep`), an instrumented repeat build completed in **~196 ms**, reporting 31 `Fresh` units, 0 `Compiling` units, and zero `rustc` process invocations.
- **Observed behavior in `toml2json`**: The October 26 real-application evaluation reported a median instrumented repeat build of **17.539 s** (spread: [15.774, 19.263] s), removing 248 target files (42.5 MiB) via `cargo clean` and recompiling 23 Cargo units on every no-change invocation.

### 1.2 Environment and Provenance
- **Host**: Linux fedora 6.19.10-300.fc44.x86_64, x86_64
- **Rustc**: `rustc 1.99.0 (b940084d7 2026-09-28)` (commit `b940084d7eb6a299eb4bfeb8e34901bc051e7ac4`)
- **Cargo**: `cargo 1.99.0 (5f94df478 2026-08-27)`
- **Tool revision**: `3f2772172ea437b9183f8d15973dcdb4d80781ab` (HEAD)
- **Tool binaries**:
  - **Unmodified baseline binary** (HEAD revision `3f27721`): SHA-256 `f0214c6deb97bafc98823a6f5101af0a6f941c95885dc0cb8a9d1c1bc390e10d`. Used for the initial clean build (`initial-build.log.gz`) and the un-fixed repeat build (`repeat-build.log.gz`).
  - **Modified binary with prefix fix** (HEAD revision `3f27721` + uncommitted prefix-matching fix in `remove_package_stamps`): SHA-256 `e4bf96cd0ee94b3382b5ed6e8bea31c04aef0976b39ba459e2716d28faf00b57`. Used for the fixed repeat build (`repeat-build-fixed.log.gz`).
- **Application**: `woodruffw/toml2json`, revision `840c3286589c7430c0315949d73e875592edac60` (MIT)
- **Workload input**: `test_input.toml` (140 bytes, SHA-256 `34e0fe98696144e7e60b2eb25e7966ef4b14d3394747ec7f21287e02e525164f`)
- **Command**: `cargo-instrument --with-dependencies -- build --release --locked --offline --target-dir <target_dir>`

### 1.3 Live Reproduction
In a fresh reproduction checkout using the preserved evaluation patch and lockfile:
1. **Initial clean build** (unmodified binary `f0214c6...`): Completed cleanly in 34.2 s. All 3 instrumented crates (`anstream`, `anstyle_query`, `clap_lex`) received transformed stamps in `release/deps/`.
2. **Immediate no-change rebuild** (unmodified binary `f0214c6...`):
   - **Wall-clock time**: **13.673 s** (user: 28.337 s, sys: 2.856 s)
   - **Files removed**: `Removed 248 files, 42.5MiB total` via `cargo clean`
   - **Units recompiled**: Exactly 23 units recompiled from source with release LTO.
3. Raw reproduction logs and structured evidence are archived under [`evidence/phase3-real-app-toml2json-rebuild-investigation-2026-11-05/`](../../evidence/phase3-real-app-toml2json-rebuild-investigation-2026-11-05/).

---

## 2. Complete Causal Determination

Tracing the execution path through package selection, pre-pass artifact capture, stamp certification, invalidation, and the final wrapped build reveals two distinct mechanisms: an architectural conflict in cache certification of skipped crates, and a concrete defect in stamp prefix matching.

### 2.1 Package Selection (`desired_instrumented_package_ids`)
When `--with-dependencies` is provided, `cargo-instrument` exports `CARGO_INSTRUMENT_DEPENDENCIES=1` and `CARGO_INSTRUMENT_REGISTRY=1`.
In `desired_instrumented_package_ids`:
- Cargo metadata reports **66 total packages** in `toml2json`'s resolved workspace graph.
- `wrapper_excluded_package_ids` contains 45 packages: the `toml2json` workspace root (1 package) and the transitive dependency closure of `opentelemetry` (44 packages).
- Packages are selected if: `target_reachable_package_ids.contains(id)` AND NOT `wrapper_excluded_package_ids.contains(id)` AND NOT telemetry/tool crates (`opentelemetry*`, `otel-shim`, `cargo-instrument`) AND (`source.is_null() || registry_enabled`).
- The remaining **40 reachable registry packages** in `toml2json`'s dependency graph are all added to `desired_instrumented_ids`, regardless of whether their source or AST satisfies instrumentation requirements.

### 2.2 Intentional Skips vs. Expected Instrumentation (Reachable vs. Compiled)
While 40 packages are selected in metadata, Cargo's compilation graph on `x86_64-unknown-linux-gnu` builds only **24 packages**. The other 16 reachable packages are Windows-specific crates (`windows-sys`, `windows-targets`, `anstyle-wincon`) or unselected optional features (`indexmap`) that Cargo filters out of the target build graph.

Of the **24 packages actually compiled by Cargo on Linux**:
- **Expected and instrumented (3 crates)**:
  - `anstream`: 8 functions transformed via native R-4 emitter.
  - `anstyle_query`: 2 functions transformed in `windows.rs` via native R-4 emitter.
  - `clap_lex`: 43 functions transformed via native R-4 emitter.
- **Intentionally skipped by compiler wrapper (21 crates)**:
  - **`#![no_std]` gate (§12.3)** (17 crates): `anstyle`, `anstyle-parse`, `anyhow`, `colorchoice`, `itoa`, `memchr`, `serde`, `serde_core`, `serde_json`, `serde_spanned`, `toml`, `toml_datetime`, `toml_parser`, `toml_writer`, `utf8parse`, `winnow`, `zmij`.
  - **`#![forbid(unsafe_code)]` gate (R26)** (3 crates): `clap`, `clap_builder`, `strsim`.
  - **Zero AST candidate functions** (1 crate): `is_terminal_polyfill`.

### 2.3 Transformed-Stamp Certification Contract
In `cargo-instrument/src/wrapper.rs` (lines 457–459):
> *"Only a successful compile from a mirrored source tree may certify an artifact as transformed. Skipped and fail-open invocations intentionally leave no marker."*

Because skipped packages compile unmodified original source, the compiler wrapper intentionally emits **zero transformed stamp files** (`.cargo-instrument-transformed-<name>*.stamp`) for them.

### 2.4 Pre-Pass Invalidation Trigger (`uninstrumented_fresh_package_ids`)
During the repeat build:
1. `acquire_native_artifacts` executes the pre-pass (`cargo build --message-format=json-render-diagnostics`).
2. Cargo reports all compiled packages as `"fresh": true`.
3. `uninstrumented_fresh_package_ids` inspects every fresh message whose `package_id` is in `desired_instrumented_ids`.
4. It requires that each fresh artifact has a corresponding `.cargo-instrument-transformed-<norm>-<hash>.stamp` whose modification time is $\ge$ the artifact's mtime.
5. Because the 21 skipped packages have no stamp file, `uninstrumented_fresh_package_ids` declares **all 21 skipped packages uninstrumented and dirty**.
6. `acquire_native_artifacts` passes these 21 packages to `invalidate_packages()`.

### 2.5 Invalidation Stamp Prefix Bug
In `invalidate_packages` (lines 1125–1144 before fix):
```rust
let prefixes: Vec<String> = package_keys
    .iter()
    .map(|pkg| format!(".cargo-instrument-transformed-{}", pkg.replace('-', "_")))
    .collect();
```
- When `clap` (a skipped crate) was cleaned, `prefix` was `".cargo-instrument-transformed-clap"`.
- This prefix string matched `".cargo-instrument-transformed-clap_lex-cb0e465c5133c9da.stamp"`.
- Similarly, when `anstyle` was cleaned, its prefix `".cargo-instrument-transformed-anstyle"` matched `".cargo-instrument-transformed-anstyle_query-a4a3723217b36db2.stamp"`.
- Consequently, **valid transformed stamps for `clap_lex` and `anstyle_query` were inadvertently purged from the filesystem** whenever sibling packages sharing a name prefix were invalidated.

### 2.6 Cargo Dependency Propagation and Rebuilds
`invalidate_packages` executes `cargo clean -p <name>` for all 21 skipped packages.
This deletes their compiled `.rlib`s, `.rmeta`s, and `.fingerprint` caches.
When the subsequent wrapped Cargo build executes:
- Cargo is forced to recompile all 21 cleaned packages.
- Cargo rebuilds all downstream crates that depend on the cleaned packages: `toml2json` depends on `toml`, `serde_json`, and `clap`; `clap` depends on `clap_builder` and `clap_lex`.
- Because `clap_lex`'s stamp was deleted by the prefix bug, `clap_lex` was also treated as dirty in subsequent invocations.
- In total, 23 crates recompiled from source, triggering full release LTO.

### 2.7 Time Attribution
- In-process metadata and session planning: **~57 ms** (inferred from fixture profiling)
- Pre-pass artifact build (`cargo build`): **~65–80 ms** (inferred from subprocess timing)
- Selective package invalidation (`cargo clean`): **~150–200 ms** (inferred from subprocess timing)
- Recompilation of 23 units with release LTO: **~13.3 s** (>98% of total wall time; measured wall clock delta)
- *Note*: Individual phase breakdowns are inferred from subprocess boundaries and prior fixture benchmarks; the tool does not currently record internal high-resolution phase telemetry. The repeat build duration under skipped certification remains an **unmeasured hypothesis** pending implementation and empirical measurement.

### 2.8 Why the Behavior Differs from the October 9 Fixture
| Dimension | October 9 Fixture (`bench_overhead`) | `toml2json` Real Application |
|---|---|---|
| Dependency types | 1 path dependency (`bench_dep`), local telemetry | 40 reachable registry crates (24 compiled on Linux), local telemetry |
| Desired instrumentation set | 1 package (`bench_dep`) | 40 packages |
| Skipped packages in desired set | **0 packages** | **21 packages** (`#![no_std]`, `forbid(unsafe)`, 0 candidates) |
| Missing stamp count | 0 | 21 |
| Packages passed to `cargo clean` | **0** | **21** |
| Fresh units in wrapped build | 31 Fresh (0 Compiling) | 0 Fresh among cleaned closure (23 Compiling) |
| Total repeat-build duration | ~196 ms (measured) | ~13.7–17.5 s (measured repeat rebuild) |

---

## 3. Implemented Defect Fix: Stamp Removal Prefix Delimiting

### 3.1 The Fix
The stamp prefix removal in `invalidate_packages` has been corrected and extracted into `remove_package_stamps`. Transformed unit stamps always take the form `.cargo-instrument-transformed-{norm}-{hash}.stamp` or `.cargo-instrument-transformed-{norm}.stamp`. Suffixing the prefix check with `-` and checking exact `.stamp` names isolates crate names from sibling crates sharing prefixes:

```rust
fn remove_package_stamps(target_dir: &Path, package_names: &[&str]) {
    let prefixes: Vec<String> = package_names
        .iter()
        .map(|pkg| format!(".cargo-instrument-transformed-{}-", pkg.replace('-', "_")))
        .collect();
    let exact_stamps: std::collections::HashSet<String> = package_names
        .iter()
        .flat_map(|pkg| {
            let norm = pkg.replace('-', "_");
            [
                format!(".instrumented_{norm}"),
                format!(".cargo-instrument-transformed-{norm}.stamp"),
            ]
        })
        .collect();
    // ...
}
```

### 3.2 Verification & Regression Test
- Added regression test `package_stamp_removal_preserves_sibling_prefixes` in `cargo-instrument/src/main.rs`.
- Tested that invalidating `clap` removes `clap` stamps but preserves `clap_lex` stamps; invalidating `anstyle` removes `anstyle` stamps but preserves `anstyle_query` stamps.
- Verified on a live repeat build that `clap_lex` and `anstyle_query` stamps now survive across rebuilds.
- All existing and new tests pass cleanly (`cargo test -p cargo-instrument`).

---

## 4. Architectural Decision: Certification of Skipped Dependencies

While the stamp removal prefix defect is resolved, the repeat build of `toml2json` still takes ~13.7 s because the 21 intentionally skipped crates are repeatedly declared uninstrumented and cleaned by `acquire_native_artifacts`.

Resolving this requires an architectural decision on **how skipped packages are certified, how their cache identity is bound, and where eligibility is evaluated**. Per Rule 6, this decision is paused for direction before implementation.

### 4.1 Invariant and Contract Analysis

#### 4.1.1 Skip Reason Stability
A marker certifying that a package was legitimately skipped must only be written for **deterministic, source-inherent properties**:
- **Stable skip reasons (Certifiable)**:
  - `#![no_std]` attribute (§12.3): Crate AST root explicitly declares `#![no_std]`, precluding standard runtime telemetry injection.
  - `#![forbid(unsafe_code)]` attribute (R26): Crate explicitly forbids `unsafe`, precluding the standard emitter's memory-safe runtime pointers.
  - Zero AST candidate functions: Discovery parser found zero non-inlined, non-generic functions matching transformation criteria (e.g. `is_terminal_polyfill`).
- **Transient and fail-open reasons (Non-Certifiable)**:
  - Syntactic syn parse errors or source reading failures.
  - Disk exhaustion or file system I/O errors.
  - Missing OpenTelemetry runtime artifacts.
  - Compiler wrapper subprocess errors.
  *Contract*: Transient failures must strictly trigger fail-open compilation without emitting any certification marker.

#### 4.1.2 Identity and Binding Requirements
If a skipped marker is emitted, it must bind strictly to:
1. **Exact Cargo unit identity**: The normalized crate name combined with Cargo's `-C extra-filename` artifact hash. A crate name alone is insufficient across profile, feature, or target variants.
2. **Compiler and tool revision**: The tool's binary identity or revision hash, ensuring that a tool upgrade with altered skipping rules automatically invalidates stale markers.
3. **Target artifact mtime**: The marker's mtime must be $\ge$ the output artifact's mtime (`.rlib`/`.rmeta`).

#### 4.1.3 Mutation and Invalidation Behavior
- **Plain `cargo build` run between instrumented builds**: Plain Cargo recompiles the unit without the wrapper. The artifact mtime becomes strictly newer than the skipped marker. In the subsequent instrumented build, `uninstrumented_fresh_package_ids` observes `artifact.mtime > marker.mtime`, rejects the marker, and invalidates the unit.
- **Source edit or feature change**: Cargo changes the `-C extra-filename` hash or detects dirty inputs, rebuilding the crate. Old markers do not match the new artifact hash.
- **Interrupted compilation**: The marker must only be created *after* `rustc` successfully exits with status 0. If `rustc` fails or is terminated mid-flight, no marker is created.

#### 4.1.4 Transformed-Stamp Contract Limitations
The current transformed-stamp contract (`wrapper.rs:457–459`) states:
> *"Only a successful compile from a mirrored source tree may certify an artifact as transformed. Skipped and fail-open invocations intentionally leave no marker."*
Furthermore, the mtime verification in `uninstrumented_fresh_package_ids`:
```rust
stamp_mtime + std::time::Duration::from_secs(1) >= *mtime
```
uses a 1-second grace window to absorb filesystem timestamp quantization. While this protects against clock skew, it cannot detect an external plain `cargo build` that finishes within the same second unless nanosecond timestamps are compared or content hashes are tracked.

### 4.2 Candidate Options & Tradeoffs

#### Option A: Persistent "Skipped / Untransformed" Marker File
- **Design**: When the compiler wrapper intentionally skips a package for a stable reason, rustc compiles the unit unmodified and the wrapper writes a distinct marker: `.cargo-instrument-skipped-{norm}-{hash}.stamp`.
- **Pre-pass check**: `uninstrumented_fresh_package_ids` considers a fresh unit certified if *either* a valid transformed stamp *or* a valid skipped stamp is present and up-to-date with the artifact mtime.
- **Tradeoffs**:
  - *Pros*: Eliminates repeated invalidation of skipped crates at its root. Requires no metadata-phase AST parsing. Retains the wrapper as the authoritative judge of syntactic eligibility.
  - *Cons*: Modifies the stamp certification contract (ADR-007 / `wrapper.rs:457`), introducing a second marker category that must be managed and cleaned across all invalidation paths.

#### Option B: Metadata-Level AST Eligibility Pre-filtering
- **Design**: Filter `desired_instrumented_package_ids` during session planning by inspecting crate root attributes (`#![no_std]`, `#![forbid(unsafe_code)]`) before `acquire_native_artifacts` runs.
- **Pre-pass check**: Skipped crates are omitted from `desired_instrumented_ids` upfront, so `uninstrumented_fresh_package_ids` never looks for their stamps and never cleans them.
- **Tradeoffs**:
  - *Pros*: Preserves the existing stamp certification contract (only transformed units have stamps).
  - *Cons*: Moves AST attribute inspection into the CLI session planning pre-pass, duplicating logic or requiring syn parsing of registry dependency root files before Cargo builds them. Does not handle crates with zero candidate functions unless full discovery runs during planning.

#### Option C: Defer Registry Dependency Invalidation to Wrapper Mode (Why It Fails)
- **Design**: Omit `cargo clean -p` during `acquire_native_artifacts` for registry dependencies, allowing the wrapper to dynamically decide whether to transform or pass through during the wrapped build.
- **Failure mechanism**: Cargo calculates freshness *before* invoking wrappers. If `cargo clean -p` is omitted, Cargo sees the existing unmodified `.rlib` as fresh and skips invoking the wrapper entirely! Downstream units would link against uninstrumented artifacts without the wrapper ever being called.

#### Option D: Status Quo (Documented Limitation)
- **Design**: Leave the current invalidation behavior in place. Real-world applications with skipped registry dependencies recompile skipped packages on repeat builds (~13.7 s on `toml2json`).
- **Tradeoffs**: Preserves absolute fail-open safety and existing contracts without new architectural surface area.

### 4.3 Validation Transition Matrix
Any implementation of Option A or B must satisfy this observable test matrix:

| Starting State | Action | Expected Invalidation | Expected Compiling Units | Final State |
|---|---|---|---|---|
| 0. Clean target | `cargo-instrument build` | None | All (24 units) | Instrumented + Certified Skipped |
| 1. State 0 | Immediate repeat `cargo-instrument build` | **None** | **0 units (all Fresh)** | Instrumented + Certified Skipped |
| 2. State 1 | Plain `cargo build` | None | 0 units (Cargo sees fresh) | Unchanged |
| 3. State 1 | `touch` a skipped crate source / `cargo clean -p <skipped>` | None | 1 skipped + downstream | Certified Skipped refreshed |
| 4. State 1 | Plain `cargo build --force` | None | Recompiled plain units | Artifact mtime > marker mtime |
| 5. State 4 | `cargo-instrument build` | Re-invalidation of plain units | Transformed + Skipped re-certified | Instrumented + Certified Skipped |

### 4.4 Recommendation & Paused Decision Point
**Recommendation**: **Option A (Persistent Skipped Marker File)** is the most robust and idiomatic design. It keeps AST eligibility discovery colocated with compilation inside the wrapper, avoids duplicating syn parsing during CLI planning, and cleanly handles zero-candidate crates.

**Direction Requested**: Does the maintainer approve introducing Option A (emitting `.cargo-instrument-skipped-{norm}-{hash}.stamp` for stable skip reasons), or should Option B (metadata pre-filtering) or Option D (status quo) be pursued?

---

## 5. Next Steps for Phase 3

1. **Awaiting direction**: Direction on Options A, B, or D to resolve skipped dependency invalidation.
2. **Next bounded evaluation item**:
   - Section 3.1 & 3.7: Independent static and dynamic coverage census for `toml2json` dependencies (dispositioning every function in `clap_lex`, `anstream`, `anstyle_query`, and skipped crates).
   - Section 3.6: Async application workloads (evaluating Tokio runtime context propagation and span correctness on a multi-threaded service fixture).

