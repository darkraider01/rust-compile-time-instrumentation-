# Historical Phase 2 Spikes

These files are **historical experiments, not production tooling or automated
test coverage**. They are intentionally outside the Cargo workspace and exist
only to preserve the executable evidence referenced by Phase 2 ADRs.

| File | Evidence retained | Current status |
| --- | --- | --- |
| `adr012-lint-span-probe.rs` + fixture | HIR body-span reachability, including the original `#[async_trait]` investigation | Superseded for Cargo-fix feasibility by ADR-013's successful temporary-driver experiment; retained for historical reachability evidence |
| `r4-extern-injection-wrapper.rs` | `--extern` artifact-ordering investigation | P2.4 question remains open; this does not implement or select an injection architecture |

Do not add these files to the workspace or invoke them as user-facing commands.
Production implementation must follow [ADR-013](../decision-records.md#adr-013---p23p24-architecture-freeze-and-cargo-fix-integration).
