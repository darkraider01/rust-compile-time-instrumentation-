← [Phase 2 - Production Hardening](README.md) · [Contents](../../README.md) · [Phase 0 ADRs (ADR-001…006)](../research/17-decision-records.md) →

---

# Phase 2 Architecture Decision Records

Continues the numbering established in [§17 (ADR-001…006)](../research/17-decision-records.md). That file is the frozen Phase 0 record and is not amended; decisions taken during Phase 2 are recorded here instead.

The format is unchanged: what was chosen, what it was chosen over, what evidence forced the choice, what it costs, and **what would make us revisit it**. The revisit condition is the important half - a decision with no falsifier is a preference.

| ADR | Decision | Status | Milestone |
| --- | --- | --- | --- |
| [007](#adr-007---cargo-is-the-scheduler-c-metadata-is-the-identity) | Cargo is the scheduler; `-C metadata` is the unit identity | **Accepted** | P2.1 |
| [008](#adr-008---the-session-plan-is-resolved-once-and-fingerprinted) | The session plan is resolved once and fingerprinted, not re-derived per unit | **Accepted** | P2.1 |
| [009](#adr-009---explicit-instrumentation-wins-at-whole-function-granularity) | Explicit instrumentation wins at whole-function granularity | **Accepted** | P2.2 |
| [010](#adr-010---hybrid-parenting-is-delegated-to-tracing-opentelemetry) | Hybrid parenting is delegated to `tracing-opentelemetry`'s context activation | **Accepted** | P2.2 |
| [011](#adr-011---the-tier-2-c-abi-is-provisional) | The Tier-2 C ABI is provisional, not the intended endpoint | **Accepted, provisional** | P2.2 → P2.3 |

---

### ADR-007 - Cargo is the scheduler; `-C metadata` is the identity

**Status:** Accepted, P2.1. Retires the proposed in-tool dependency scheduler.

#### Context

Phase 1 keyed instrumented source mirrors on `--crate-name`. That key is not unique: Cargo reuses the same `--crate-name` across multiple versions of one package (`foo v1` and `foo v2` in one graph) and across distinct targets of one package (`src/lib.rs` and `tests/it.rs`). Two units then write to the same mirror directory - producing silent miscompilation, or an OS file-sharing crash when the writes race (G1, G3).

The initial reading of that failure was that the tool needed to understand build ordering, and therefore needed a scheduler of its own.

#### Options considered

1. **Build a dependency scheduler inside `cargo-instrument`** - topological ordering, parallelism control, unit deduplication, freshness tracking. Rejected.
2. **Disambiguate the mirror key from data Cargo already passes in argv.** Chosen.
3. **Serialize all instrumentation behind a global lock.** Rejected - converts a correctness bug into a build-time bug, and does not fix the version-collision case where both units are legitimately concurrent.

#### Evidence

- **[Fact]** Cargo already owns DAG topological ordering, parallelism, unit deduplication, and freshness. A second scheduler would duplicate all four and could only disagree with the first.
- **[Fact]** Cargo passes `-C metadata=<hash>` to every `rustc` invocation, and that hash is globally unique per compilation unit - it already distinguishes package version *and* target kind, which is exactly the discrimination `--crate-name` lacks.
- **[Fact]** The two reproduced defects (G1, G3) are both *identity* failures, not *ordering* failures. No observed defect required knowing what compiles before what.

The load-bearing argument for option 1 was that the tool needed build-graph awareness. It needed unit identity, which is a strictly smaller thing, and which Cargo hands over for free.

#### Decision

**`UnitId = (crate_name, metadata_hash)`, recovered from `-C metadata` in rustc argv. One private mirror directory per `UnitId`, written via temp file + atomic rename. `cargo-instrument` schedules nothing.**

#### Consequences

- ✅ **G1 and G3 close together** - version collisions and lib/test collisions are the same bug under one fix.
- ✅ **No scheduler to build, test, or keep in sync with Cargo's.** The largest piece of speculative machinery proposed for Phase 2 was never written.
- ✅ **Atomic rename removes the concurrent read/write window** without a lock, so parallelism stays Cargo's to control.
- ❌ **A hard dependency on an unstable-ish rustc flag surface.** If Cargo ever stops passing `-C metadata`, unit identity degrades to the Phase 1 key and G1/G3 return.
- ⚠️ **Mirror directory count grows with units, not packages.** A 105-unit graph produces 105 mirrors; disk cost is now linear in build width rather than graph size.
- ⚠️ **The mechanism is stable and public, but its production track record is caching, not rewriting.** `RUSTC_WRAPPER` has been a documented Cargo feature since [cargo#3887](https://github.com/rust-lang/cargo/pull/3887) (2017) and carries `sccache` in production widely, so the concern is not stability. It is that every well-adopted user of this hook *caches* compilations; none *rewrites source* before handing off. Maintainer review (2026-09-10) reached for prior art here and the honest answer is that there is little for the rewriting half. Recorded as a known-thin area rather than a resolved one.

#### Revisit if

- Cargo stops emitting `-C metadata`, or emits a value that is no longer unique per unit. The regression tests `test_multiple_versions_of_one_package_must_not_share_a_mirror` and `test_lib_and_test_units_must_not_share_a_mirror` are the tripwires.
- A defect appears that genuinely requires *ordering* knowledge rather than identity. None has yet.

---

### ADR-008 - The session plan is resolved once and fingerprinted

**Status:** Accepted, P2.1, hardened in P2.1 closeout (D1, D3, D4, D5).

#### Context

Two policy questions must be answered before any unit is instrumented: is `otel-shim` reachable in this graph (or will Tier-2 trampolines fail to link, G4), and is this package host-only (or will a proc-macro dependency receive trampolines it cannot resolve, G2)?

Both are properties of the whole build graph. The wrapper, however, runs once per unit and sees only one unit's argv.

#### Options considered

1. **Query `cargo metadata` per wrapper invocation.** Correct, and catastrophically slow - N units means N metadata queries, all racing on a cold cache.
2. **Query once in the CLI, pass the result to every child by environment variable.** Chosen.
3. **Cache on disk with a time-based staleness window.** Implemented first, then replaced - see below.

#### Evidence

- **[Fact, D4]** On a cold cache, every wrapper process independently shelling out to `cargo metadata` produces a query storm proportional to build width.
- **[Fact, D1]** The first cache used a 1800-second mtime window. A manifest edited inside that window was invisible: the build ran on a stale policy with no signal. A time window is not a freshness test, it is a guess about how fast people edit files.
- **[Fact, D3]** `find_best_manifest_dir` is a heuristic over the current directory and `--out-dir`. It can select the wrong manifest when a decoy sibling manifest exists, or when `--manifest-path` points outside the cwd - and it did.
- **[Fact, D5]** When `cargo metadata` fails outright, the safe direction is to instrument nothing rather than to assume a shim provider exists.

#### Decision

**The CLI resolves the `SessionPlan` once - respecting `--manifest-path` and `--target-dir` rather than guessing - writes it to disk, and exports the path via `CARGO_INSTRUMENT_SESSION`. Wrappers load it. Freshness is a SHA-256 content hash over `Cargo.lock` and every local manifest, not elapsed time. On metadata failure the plan defaults to `has_otel_shim_provider: false`.**

#### Consequences

- ✅ **One metadata query per build** instead of one per unit.
- ✅ **Cache invalidation is exact.** A manifest or lockfile edit changes the hash; nothing else does.
- ✅ **The heuristic is demoted to a fallback** for raw `RUSTC_WRAPPER` invocations, where no CLI ran and there is genuinely nothing better to do.
- ❌ **Every unit re-hashes the lockfile and manifests** to check freshness. Measured at ~30KB on this workspace - negligible, but it is per-unit work that the mtime check did not do.
- ❌ **The failure default is silent under-instrumentation.** A broken `cargo metadata` yields a build with no spans and a warning, not an error.

#### Revisit if

- The fingerprint hash becomes a measurable share of build time on a large graph (P2.4 will measure this). The fix is to hash once in the CLI and pass the digest, not to return to mtime.
- Under-instrumentation on metadata failure proves harder to notice in practice than a hard failure would be. The warning is currently the only signal.

---

### ADR-009 - Explicit instrumentation wins at whole-function granularity

**Status:** Accepted, P2.2. Implements S10 ([§16](../research/16-instrumentation-semantics.md)).

#### Context

A function may already carry `#[tracing::instrument]`, `#[instrument]`, a `#[propagate_context]` attribute, or a hand-written `.with_context(cx)` / `tracer.start(...)` body. Automatically instrumenting it again produces a duplicate span, and in the `#[propagate_context]` case a second `__otel_cx` binding that shadows the first - a compile warning at best, and under `#![deny(warnings)]` a build failure.

#### Options considered

1. **Instrument anyway and deduplicate at export time.** Rejected - the collision is a *compile-time* identifier clash, which no exporter can undo.
2. **Detect explicit instrumentation precisely and splice around it.** Rejected for P2.2 - requires reasoning about which statements in a body are context-bearing, on partially-macro-expanded source.
3. **Skip the entire function if anything explicit is found anywhere in it.** Chosen.

#### Evidence

- **[Fact]** Detection is a whole-body scan for attributes and known call shapes; it is conservative by construction and can only over-skip, never under-skip on the patterns it knows.
- **[Measured]** Over-suppression on real crates is **0.0%** - `census-0.4.2` (32 functions) and `async-trait` (55 functions) contain no handwritten OTel, so nothing is skipped that would otherwise be instrumented.
- **[Fact]** `test_propagate_context_collision_hazard_prevented` compiles a fixture under `#![deny(warnings)]`, making the compiler itself the oracle for the shadowing hazard rather than a string assertion.
- **[Fact]** The universal reconciliation identity `candidates + skipped = total_functions` holds across the widening, so over-skipping is always *counted*, never silent.

#### Decision

**Functions bearing span-creating attributes (`#[instrument]` and qualified forms) or context-propagating attributes (`#[propagate_context]`), or containing manual span creation in their body, are skipped entirely by `ast.rs`. Attribute matching is on the path's final segment, so `#[tracing::instrument]`, `#[tracing_attributes::instrument]`, and `#[otel_instrument::instrument]` all match.**

#### Consequences

- ✅ **The collision class is closed by construction**, not by careful splicing that must stay correct as bodies change.
- ✅ **Measured coverage cost is zero** on the crates tested so far.
- ✅ **Every skip is attributed** in `skipped_stats`, so the cost is observable rather than assumed.
- ❌ **A function with one hand-written span loses automatic instrumentation for its whole body**, including call sites that would not have collided.
- ❌ **`#[cfg_attr(feature = "x", tracing::instrument)]` is not detected** - the path's final segment is `cfg_attr`. Under that feature the function is double-instrumented.

#### Revisit if

- Measured over-suppression on a real-world graph exceeds a few percent. Then precise detection (option 2) has to earn its complexity.
- `cfg_attr`-gated instrumentation turns out to be a pattern people actually use.

---

### ADR-010 - Hybrid parenting is delegated to `tracing-opentelemetry`

**Status:** Accepted, P2.2. Held conditionally when taken; upgraded to Accepted after maintainer
review confirmed the upstream behaviour is deliberately maintained rather than incidental.

#### Context

A caller annotated `#[tracing::instrument]` and an automatically instrumented dependency must produce one trace, not two roots. The dependency span is created via the Tier-2 C ABI and parents itself from `Context::current()`. For that to be the caller's span, something must have attached the caller's `tracing` span to the OTel context.

#### Options considered

1. **Have the tool inject explicit context propagation at the boundary.** Rejected - the tool does not control the caller's `#[tracing::instrument]` expansion, and injecting around it re-opens the collision class ADR-009 just closed.
2. **Rely on `tracing-opentelemetry`'s `with_context_activation: true` default**, which activates `Context::current()` inside instrumented spans. Chosen.
3. **Require users to configure activation explicitly.** Rejected as a usability cost for the default path.

#### Evidence

- **[Fact]** `test_tracing_opentelemetry_activates_context_by_default` asserts the activation happens with no configuration, pinning the behaviour we depend on.
- **[Fact]** `test_sync_hybrid_parenting` and `test_async_trait_hybrid_parenting` assert `dep_span.parent_span_id == caller_span.span_id` end-to-end in subprocess fixtures, including across the `#[async_trait]` desugaring boundary.
- **[Fact]** `otel_shim::active_span_count() == 0` after each fixture, so the parenting is achieved without leaking handles.
- **⚠️ [Fact]** The activation default lives in `tracing-opentelemetry`, not in this project. Upstream [opentelemetry-rust-contrib#791](https://github.com/open-telemetry/opentelemetry-rust-contrib/issues/791) is still open in this area.
- **Maintainer, Scott Gerring (`#otel-rust`), 2026-09-10:** *"the context activation interop between tracing-opentelemetry and the otel context works well now, as we (mainly a colleague of mine at datadog and a bit of my own work) spent a bunch of time fixing it up."* This is the material update: the default is not an incidental behaviour that might drift, it is a maintained interop path with named owners who invested in it.

#### Decision

**Do not inject boundary propagation. Depend on `tracing-opentelemetry`'s default context activation, and pin that dependency with a test that fails if the default changes.**

#### Consequences

- ✅ **Hybrid parenting works with zero user configuration** on the default path.
- ✅ **No new injection at the explicit/automatic boundary**, so ADR-009's guarantee is not weakened.
- ❌ **A structural behaviour of this project is owned by an upstream default we do not control.** If it flips, hybrid traces silently split into two roots. Downgraded from High to Low likelihood on the maintainer statement above - the dependency is unchanged, the probability of it moving unannounced is not.
- ⚠️ **The pinning test detects the change at our CI, not at the user's build.** A user on a newer `tracing-opentelemetry` than our lockfile gets no warning.

#### Revisit if

- `test_tracing_opentelemetry_activates_context_by_default` fails on a dependency bump. That is the tripwire, and it should be treated as an architecture event, not a test fix.
- contrib#791 lands and changes activation or propagation semantics.
- A runtime assertion at `otel-shim` init proves cheap enough to move detection from our CI to the user's build.

---

### ADR-011 - The Tier-2 C ABI is provisional

**Status:** Accepted, provisional, P2.2. Does not reverse [ADR-003](../research/17-decision-records.md#adr-003--extern-c-trampolines-for-dependency-coverage), but withdraws the assumption that it is the endpoint. Opened by maintainer review, 2026-09-10.

#### Context

[ADR-003](../research/17-decision-records.md#adr-003--extern-c-trampolines-for-dependency-coverage) chose `extern "C"` trampolines for dependency coverage on one premise: a dependency crate cannot name `opentelemetry`, because doing so would require editing its `Cargo.toml`, which the tool does not do. The C ABI was the way around a dependency edge we believed we could not create.

Maintainer review put pressure on both halves of that - the cost of the C ABI, and whether the premise was ever true.

#### Evidence

- **Maintainer, Scott Gerring (`#otel-rust`), 2026-09-10:** *"ending up C FFI boundaries everywhere through the call stack is probably a non starter ... it breaks panic handling at least, and it will probably break a pile of optimisations too, in one part because it forces the C calling convention to be used."*
- **[Fact, verified]** All nine exported symbols in `otel-shim/src/lib.rs` and both spliced declarations in `transform.rs` are plain `extern "C"`, not `extern "C-unwind"`. Since Rust 1.71 a panic that reaches a plain `extern "C"` boundary **aborts the process**; it is not catchable by `catch_unwind` in the host application.
- **[Fact, already hit once]** `otel-shim/src/lib.rs` carries a fix comment for exactly this class of bug: *"running it while `STACK` is still borrowed panics with 'RefCell already borrowed'."* A `RefCell` double-borrow inside `__otel_span_exit` is a failure mode this project has already found and repaired once. The abort path is therefore reachable in practice, not in theory.
- **[Fact]** The blast radius is a process abort, which is strictly worse than the uninstrumented behaviour it replaces. An application that isolates panics per request - the common shape for a web server - loses the whole process instead of one request. This directly contradicts S11, under which every failure path warns and compiles or runs unmodified.
- **Maintainer, same source:** *"i wonder if for the FFI you can manipulate the project model to add a dep as you are interceding with `RUSTC_WRAPPER` anyway."*

The last point is the load-bearing one. ADR-003's premise was that the dependency edge could not be created. But the tool already owns the full `rustc` argv, and `--extern <name>=<path>` creates exactly that edge without Cargo's resolver and without touching any manifest. If that works, Tier-2 does not need a C ABI at all - dependency crates would emit the same native `opentelemetry` calls Tier-1 does, and the entire tier collapses into Tier-1.

#### Decision

**Treat the Tier-2 C ABI as a working mechanism with a known expiry, not as the architecture. Two tracks:**

1. **Immediate mitigation (P2.2).** The abort path is a live defect and is fixed independently of what replaces the tier. Two options, not mutually exclusive:
   - `extern "C-unwind"` on all eleven declarations, so a panic propagates instead of aborting. Restores the behaviour a normal Rust dependency would have had.
   - `catch_unwind` inside each exported function, returning the no-op handle `0` on panic. Strictly more aligned with S11 - telemetry that fails should degrade, not propagate into user code that never asked for it.

   The tension is real: `"C-unwind"` is correct, `catch_unwind` is fail-open, and S11 argues for the second. Recorded here rather than settled, because it is a policy call.

2. **Replacement investigation (P2.3).** Spiked 2026-09-10; results below. `--extern` injection is viable. The blocker is not the one this ADR originally named.

#### Spike result (2026-09-10)

A minimal `RUSTC_WRAPPER` was built that appends `--extern opentelemetry=<rlib>` when it sees `--crate-name dep_lib`, against a workspace where `dep_lib`'s manifest declares no `opentelemetry` dependency at all.

- **✅ Crate-instance identity is not a problem.** The injected instance is the same one Cargo resolved. A `Context` constructed in the app was accepted by `dep_lib::takes_context(&cx)`, and a `Context` returned from `dep_lib::make_context()` bound to the app's own `opentelemetry::Context` annotation. Both directions compile, link and run. The question this ADR was blocked on is answered favourably.
- **❌ Build ordering is the real blocker.** On a cold build `dep_lib` compiles *before* `opentelemetry` exists, because Cargo's DAG has no edge between them. The rlib is absent at injection time and the build fails hard with `E0433: cannot find module or crate opentelemetry`.
- **✅ A pre-pass resolves the ordering problem.** Running `cargo build -p opentelemetry` into the session target directory before the main build makes the rlib present when `dep_lib` compiles. Verified on both `dev` and `release`.
- **✅ The pre-pass is not a double compile.** The main build emits no `Compiling opentelemetry` line and the rlib hash is unchanged (`bf349e284d4db059` on dev, `ebc2fd7a58a5e107` on release). Cargo's fingerprint matches and the artifact is reused, so the cost is scheduling, not recompilation. This matters because "the double perf cost" was one of the two objections raised in review.
- **✅ `-p` resolves features graph-wide, not to bare defaults.** The pre-pass and the full build produce the identical hash, so the feature-mismatch hazard is much narrower than assumed - Cargo unifies features across the workspace before building the single package.

**Not yet tested, and each could still sink it:** cross-compilation (`--target`), a graph containing two `opentelemetry` versions (the wrapper's ambiguity branch is currently a bail-out), a graph where the application does not depend on `opentelemetry` at all (the pre-pass would have nothing to build from the resolved graph), host/build-script units, and any interaction with the existing `-C metadata` unit identity from [ADR-007](#adr-007---cargo-is-the-scheduler-c-metadata-is-the-identity).

#### Consequences

- ✅ **R-1 and R-2 may become moot.** Both are limitations of the ABI's width - a hardcoded `"dependency"` scope, and discarded file/line/kind. Native calls carry all of it for free, so the P2.3 ABI-extension work should not start until the replacement question is settled.
- ✅ **The Tier-1/Tier-2 split may collapse**, retiring what [ADR-001](../research/17-decision-records.md#adr-001--generate-native-opentelemetry-api-calls) called its *"largest unpriced consequence"*.
- ❌ **The calling-convention cost is unmeasured.** The C ABI is forced at every instrumented dependency function. Two small lifecycle calls per span is plausibly noise against span creation itself, but that is an assumption, not a measurement, and it should be benchmarked rather than argued.
- ⚠️ **`--extern` injection trades one unsanctioned mechanism for another.** It creates a dependency edge Cargo did not resolve, which is a stronger intervention than reading argv. If it works, its own failure modes need their own record.

#### Revisit if

- ~~`--extern` injection is shown to produce a single shared crate instance across the graph.~~ **Met, 2026-09-10.** It does. The remaining question is no longer identity but whether the pre-pass survives the untested cases listed above. If it does, Tier-2 is replaced and this ADR is superseded by the record of that decision.
- The pre-pass fails on cross-compilation, multi-version graphs, or a graph with no `opentelemetry` of its own, in a way that has no clean workaround. Then the C ABI is the architecture after all, ADR-003 stands unqualified, and R-1/R-2 proceed as planned in P2.3.
- The calling-convention overhead is measured and turns out to be material at realistic span rates. That would raise the priority of the replacement track independently of the panic issue.

---

---

← [Phase 2 - Production Hardening](README.md) · [Contents](../../README.md) · [Phase 0 ADRs (ADR-001…006)](../research/17-decision-records.md) →
