# P2.3 compiler driver

This standalone crate is intentionally excluded from the stable workspace. It
uses the pinned `nightly-2026-09-09` toolchain named by
[`../p23-toolchain.txt`](../p23-toolchain.txt), plus `rustc-dev`, `rust-src`,
and `llvm-tools-preview` (needed for the Windows compiler-private link).

The supported workflow is repository development through:

```text
cargo instrument-rust --apply
```

The stable workspace and already-generated application source do not require
this toolchain. Installed/distributed driver discovery is intentionally not
implemented yet.
