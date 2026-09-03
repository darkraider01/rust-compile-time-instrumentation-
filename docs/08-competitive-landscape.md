← [eBPF as a Future Extension](07-ebpf-future.md) · [Contents](../README.md) · [Gap Analysis](09-gap-analysis.md) →

---

## 8. Competitive / Adjacent Landscape


Only technically relevant entries are included. "Automatic" means the user does not annotate individual functions.

| Project | Rust | Automatic | Compile-time | Source-level | Compiler-level | eBPF | OTel |
| --- | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| **`otelc`** (OTel Go compile-time) | ✗ | ✓ | ✓ | ✓ (AST) | ✗ | ✗ | ✓ |
| **OBI / Beyla** | ✓ (target) | ✓ | ✗ | ✗ | ✗ | ✓ | ✓ |
| **`J00MZ/opentelemetry-rust-instrumentation`** | ✓ | ✓ | ✗ | ✗ | ✗ | ✓ | ✓ |
| **`tracing` + `#[instrument]`** | ✓ | ✗ | ✓ | ✓ (proc macro) | ✗ | ✗ | via bridge |
| **`tracing-opentelemetry`** | ✓ | n/a | ✗ | ✗ | ✗ | ✗ | ✓ |
| **`opentelemetry-rust` SDK** | ✓ | ✗ | ✗ | ✗ | ✗ | ✗ | ✓ |
| **`tracing-orchestra`** | ✓ | partial (per module/impl) | ✓ | ✓ (proc macro) | ✗ | ✗ | via bridge |
| **`autometrics`** | ✓ | ✗ | ✓ | ✓ (proc macro) | ✗ | ✗ | ✓ (metrics only) |
| **`otel-instrument`** | ✓ | ✗ | ✓ | ✓ (proc macro) | ✗ | ✗ | ✓ |
| **Clippy** | ✓ | ✓ | ✓ | ✗ | ✓ (HIR/MIR) | ✗ | ✗ |
| **Miri / Kani / MIRAI / Flowistry** | ✓ | ✓ | ✓ | ✗ | ✓ (MIR) | ✗ | ✗ |
| **`-C instrument-coverage`** | ✓ | ✓ | ✓ | ✗ | ✓ (LLVM) | ✗ | ✗ |
| **`-Z instrument-xray`** | ✓ | ✓ | ✓ | ✗ | ✓ (LLVM) | ✗ | ✗ |
| **`sccache`** | ✓ | ✓ | ✓ | ✗ | ✗ (wrapper only) | ✗ | ✗ |
| **← our proposed project** | ✓ | ✓ | ✓ | ✓ | later | later | ✓ |

**Reading the table.** Three clusters exist and the intersection between them is empty:

1. **Rust + compile-time + OTel** — all existing entries are **not automatic** (`#[instrument]`, `autometrics`, `otel-instrument`, `tracing-orchestra`).
2. **Rust + automatic + OTel** — all existing entries are **not compile-time** (OBI, J00MZ).
3. **Rust + automatic + compile-time** — all existing entries are **not OTel** (Clippy, Miri, coverage, XRay; these are analysis or profiling tools).

**[Inference]** The unoccupied cell is precisely **Rust + automatic + compile-time + OTel**. That is the project's differentiator, and it is a real one — but note that it is a *combination* gap, not a *mechanism* gap: every individual mechanism required already exists and is proven. That is good news for feasibility and bad news for novelty claims (§9.4).

---

---

← [eBPF as a Future Extension](07-ebpf-future.md) · [Contents](../README.md) · [Gap Analysis](09-gap-analysis.md) →
