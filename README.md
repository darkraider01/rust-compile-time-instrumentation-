<div align="center">

# rust-compile-time-instrumentation

**Zero-code compile-time OpenTelemetry instrumentation for Rust — reaching third-party dependencies, on stable Rust, with no eBPF.**

[![CI](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/workflows/ci.yml/badge.svg)](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/workflows/ci.yml)
[![Integration](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/workflows/integration.yml/badge.svg)](https://github.com/darkraider01/rust-compile-time-instrumentation-/actions/workflows/integration.yml)
[![Rust](https://img.shields.io/badge/rust-stable-orange?logo=rust)](https://www.rust-lang.org)
[![No nightly](https://img.shields.io/badge/nightly-not%20required-brightgreen)](docs/research/17-decision-records.md)
[![License: Apache 2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Phase](https://img.shields.io/badge/phase-1%20in%20progress-yellow)](#status)

</div>

Instruments Rust applications *and their dependencies* at build time — no source annotations, no manual span wiring — by intercepting `rustc` via `RUSTC_WRAPPER` and splicing native OpenTelemetry calls at the byte level. Runs on stable Rust with no compiler forks, no MIR passes, and no eBPF; the frozen architecture and the evidence behind it are in [`docs/research/`](docs/research/).

## Status

| Phase | Status |
| --- | --- |
| **Phase 0 — Landscape research & architecture** | Complete. Six frozen architecture decisions ([ADR-001 … ADR-006](docs/research/17-decision-records.md)), a normative correctness spec ([§16](docs/research/16-instrumentation-semantics.md)), and a validated experiment matrix ([Appendix E](docs/research/appendix-e-experiment-matrix.md)) |
| **Phase 1 — `cargo-instrument` tool** | In progress. P1.1–P1.3 implemented: `RUSTC_WRAPPER` interception, Cargo invocation classification, and `syn`-based AST/byte-span discovery — analysis-only, zero source rewriting |

## Documentation

- **[docs/research/](docs/research/)** — the full Phase 0 investigation: landscape survey, architecture candidates, the [instrumentation semantics specification](docs/research/16-instrumentation-semantics.md) (the correctness oracle Phase 1's tests are written against), the [architecture decision records](docs/research/17-decision-records.md), and four rounds of verification (hands-on experiments, an adversarial review, a maintainer Q&A round, and a validated experiment matrix). Start at [docs/research/README.md](docs/research/README.md).
- Later phases get their own sibling folders under `docs/` as that work lands (e.g. `docs/phase1/`) — `docs/research/` is specifically the archived Phase 0 record and stays frozen once a decision in it is superseded rather than edited in place; supersessions are recorded, not silently rewritten.

## The tool

`cargo-instrument/` — the Cargo subcommand and `RUSTC_WRAPPER` implementation.

```bash
cargo build --workspace
cargo test --workspace
cargo run --bin cargo-instrument -- analyze path/to/file.rs   # standalone AST/byte-span analysis
cargo run --bin cargo-instrument -- -- build                  # wrapped build, isolated target/instrumented (ADR-004)
```

CI runs `fmt`/`clippy`/`test`/`build` across Linux, Windows, and macOS ([`ci.yml`](.github/workflows/ci.yml)), plus a dedicated real-subprocess integration workflow ([`integration.yml`](.github/workflows/integration.yml)) that documents exactly which claims about the wrapper/Cargo integration are proven by the current test suite.

## License

[Apache License, Version 2.0](LICENSE).
