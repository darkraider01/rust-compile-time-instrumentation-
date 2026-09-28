# First-Party Compiler Driver

This standalone crate generates instrumentation suggestions from compiler HIR.
It is excluded from the stable workspace because it uses compiler-private APIs.
The first-party CLI supplies it as RUSTC while Cargo fix owns RUSTC_WRAPPER.

## Setup

Install the toolchain recorded in [the toolchain pin](../p23-toolchain.txt):

```sh
rustup toolchain install nightly-2026-09-09 --component rustc-dev --component rust-src --component llvm-tools-preview
cargo build -p cargo-instrument --bins
```

Run the build command from the repository root. Make the resulting target/debug
binaries available on PATH, then run from the application workspace:

```sh
cargo instrument-rust --apply
cargo instrument-rust --apply --package my-app
```

The application must declare a compatible opentelemetry dependency and have a
clean Git worktree. The CLI builds this driver from the development checkout.
Review and commit generated edits before applying again; persistent ownership
markers prevent repeated instrumentation. Generated source builds on stable Rust.

## Supported scope

Supported forms include ordinary free functions, inherent methods, trait
implementation methods, native async functions, and verified async_trait method
bodies. Compiler type information identifies Result returns. Explicit
instrumentation takes precedence.

Nested local functions, default trait method bodies, macro-owned source, const
functions, closures, and foreign ABIs are excluded. Dependency source is handled
by the separate opt-in wrapper workflow.

Preview UX (`--show`) and packaged driver discovery remain pending. This driver
requires the pinned nightly even though the wrapper and generated source use
stable Rust. See [ADR-012 and ADR-013](../../docs/decisions/adr-007-013.md) and the
[source application tests](../../cargo-instrument/tests/source_instrumentation_apply_tests.rs).
