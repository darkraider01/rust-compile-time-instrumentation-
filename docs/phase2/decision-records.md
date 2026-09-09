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
| [010](#adr-010---hybrid-parenting-is-delegated-to-tracing-opentelemetry) | Hybrid parenting is delegated to `tracing-opentelemetry`'s context activation | **Accepted, conditional** | P2.2 |

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

**Status:** Accepted, conditional, P2.2. This is the decision with the largest unowned dependency.

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

#### Decision

**Do not inject boundary propagation. Depend on `tracing-opentelemetry`'s default context activation, and pin that dependency with a test that fails if the default changes.**

#### Consequences

- ✅ **Hybrid parenting works with zero user configuration** on the default path.
- ✅ **No new injection at the explicit/automatic boundary**, so ADR-009's guarantee is not weakened.
- ❌ **A structural behaviour of this project is owned by an upstream default we do not control.** If it flips, hybrid traces silently split into two roots.
- ⚠️ **The pinning test detects the change at our CI, not at the user's build.** A user on a newer `tracing-opentelemetry` than our lockfile gets no warning.

#### Revisit if

- `test_tracing_opentelemetry_activates_context_by_default` fails on a dependency bump. That is the tripwire, and it should be treated as an architecture event, not a test fix.
- contrib#791 lands and changes activation or propagation semantics.
- A runtime assertion at `otel-shim` init proves cheap enough to move detection from our CI to the user's build.

---

---

← [Phase 2 - Production Hardening](README.md) · [Contents](../../README.md) · [Phase 0 ADRs (ADR-001…006)](../research/17-decision-records.md) →
