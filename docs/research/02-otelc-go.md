← [Executive Summary](../../README.md) · [Contents](../../README.md) · [The Rust Compilation Pipeline](03-rust-compiler-pipeline.md) →

---

## 2. OpenTelemetry Go Compile-Time Instrumentation (`otelc`)


Primary sources:
- Repository: [open-telemetry/opentelemetry-go-compile-instrumentation](https://github.com/open-telemetry/opentelemetry-go-compile-instrumentation)
- User docs: [opentelemetry.io/docs/zero-code/go/compile-time/](https://opentelemetry.io/docs/zero-code/go/compile-time/)
- Rule schema: [docs/rules.md](https://github.com/open-telemetry/opentelemetry-go-compile-instrumentation/blob/main/docs/rules.md)
- Implementation notes: [docs/implementation.md](https://github.com/open-telemetry/opentelemetry-go-compile-instrumentation/blob/main/docs/implementation.md)
- ADRs: [docs/adr/](https://github.com/open-telemetry/opentelemetry-go-compile-instrumentation/tree/main/docs/adr)
- v1 announcement: [Announcing v1 of OpenTelemetry Go Compile-Time Instrumentation](https://opentelemetry.io/blog/2026/go-compile-time-instrumentation-v1/)

### 2.1 What it is

**[Fact]** `otelc` is the official OpenTelemetry SIG tool for injecting instrumentation into Go programs at build time. It reached v1.0.0 on 2026-07-14 and v1.1.0 on 2026-08-24 (verified via the GitHub releases API). It requires Go 1.25+, is Apache-2.0 licensed, and originated from Alibaba's and Datadog's separate efforts being merged into one vendor-neutral SIG at the start of 2025.

**[Fact]** v1 ships a deliberately narrow instrumentation set: `net/http`, `database/sql`, gRPC, Redis, and Go runtime metrics. It is explicitly "a focused set of instrumentations rather than the full breadth of the Go ecosystem."

**[Inference — scoping lesson]** The official, well-resourced, production-grade version of this idea shipped v1 with five instrumentations after roughly eighteen months of SIG work. Any Phase 1 scope larger than "generic function instrumentation plus one library integration" is unrealistic for a personal project.

### 2.2 Where it sits in the toolchain

**[Fact]** `otelc` hooks `go build` via the Go toolchain's `-toolexec` flag, which lets an external command intercept each invocation of the underlying compile/link tools. `otelc go build` is a thin wrapper that sets this up. Documented invocations:

```
./otelc go build -o myapp .                        # direct wrapping
go tool otelc go build -o myapp .                  # as a Go 1.24+ tool dependency
GOFLAGS="${GOFLAGS} '-toolexec=otelc toolexec'"    # transparent to existing build scripts
```

The key property: `-toolexec` sits **between** `go build`'s dependency planning and the actual invocation of `compile`, so `otelc` sees the source of every package in the build — the application, its third-party dependencies, and the standard library.

### 2.3 Mechanism: AST/source transformation, not compiler internals

**[Fact]** `otelc` does **not** modify the Go compiler and does not operate on Go's internal SSA IR. It intercepts the compiler invocation, parses the Go source files the compiler was about to read, rewrites their **AST**, writes modified sources out, and re-invokes the real compiler on the rewritten files.

This is the single most important design fact for us. The official, production, vendor-neutral answer to "how do you do compile-time auto-instrumentation" is **source/AST rewriting behind a build-tool hook**, not compiler-internal IR manipulation.

### 2.4 Two-phase architecture

**[Fact]** From `docs/implementation.md`:

**Phase 1 — Setup / preprocess.**
- Analyses the project's dependency graph, using `go build -n` to enumerate what will actually be compiled.
- Matches the resolved dependency set against the instrumentation rule set.
- Generates an import file pulling in the OTel SDK and the hook packages the matched rules need.
- Runs `go mod tidy` so injected dependencies are resolved *before* compilation begins.
- Writes matched rules into a `.otelc-build/` working directory.

**Phase 2 — Instrument.**
- The tool is invoked via `-toolexec` for each package compilation.
- It matches the package and its functions against the selected rules.
- It injects trampoline code into the AST.
- Because Phase 1 already made the hook packages real dependencies, symbol resolution and linking work without special handling.

**[Inference]** The two phases exist to resolve a chicken-and-egg constraint that Rust shares exactly: *you cannot inject a call to a library that is not in the dependency manifest, and you cannot know which libraries you need until you have analysed the dependency graph.* Any Rust equivalent needs the same split (§10, Architecture A).

### 2.5 The trampoline / hook mechanism

**[Fact]** `otelc` uses two-level indirection:

```
target function  →  trampoline  →  hook (in a separate, normally-compiled package)
```

The trampoline is auto-generated from hook configuration; developers do not write it. Two stated reasons:

- **Panic isolation.** The trampoline "catches panics and isolates exception handling, preventing them from affecting the target function or hook code." Instrumentation must not be able to crash the application.
- **Late binding.** The trampoline is linked to the hook implementation with `//go:linkname`, so injected code in the target package needs no normal import edge to the hook package.

**[Fact]** Hooks receive a structured `Context` giving access to function parameters, return values (readable *and* writable), and a slot carrying state from the entry hook to the exit hook.

**[Inference]** The entry→exit state slot is exactly what a span needs: create on entry, stash the handle, close on exit. Rust's natural equivalent is an RAII guard, which is *simpler* than Go's explicit slot — but it interacts badly with `?`-based early return (actually fine: `Drop` still runs) and with `async` (not fine: see §6.3).

### 2.6 How instrumentation points are identified: the rule system

**[Fact]** Instrumentation is declared in YAML rules with a two-tier schema — package selectors (`target`, `version`), point selectors (`where`), and modifiers (`do`).

| Field | Required | Meaning |
| --- | --- | --- |
| `target` | yes | Package import path or glob |
| `version` | no | Version range, `start_inclusive,end_exclusive` |
| `where` | no | Non-package selectors and file-level predicates |
| `do` | yes | Ordered list of modifiers; the modifier name determines the rule type |
| `imports` | no | Alias → package path map for injected code |
| `name` | no | Rule identifier; defaults to the YAML key |

**[Fact]** Eight rule types exist, keyed by modifier name:

1. **`inject_hooks`** — call a `before` hook at function entry and an `after` hook before return. Selectors: `func` (required), `recv` (for methods), signature filters (`signature`, `signature_contains`, `result`, `last_result`, `param`). *This is the span-creating rule.*
2. **`add_struct_fields`** — add fields to a struct type; canonical use is attaching a context field so tracing flows through its methods.
3. **`inject_code`** — inject arbitrary Go source at a matched location with `text/template` placeholders (`{{.FuncName}}`, `{{.FuncArgument N}}`, `{{.Receiver}}`). Documented as being for prototyping and debugging.
4. **`wrap_call`** — rewrite a *call site* rather than a definition. Selector `function_call` is a qualified name; the modifier can `replace` the call with a template or `append_args`.
5. **`expand_directive`** — instrument functions annotated with a magic comment (e.g. `//otelc:span`). Opt-in instrumentation with no source dependency on the tool.
6. **`add_file`** — add a whole new Go source file to the target package, for helpers other hooks need.
7. **`assign_value`** — replace or wrap a package-level `var`/`const` initializer. Canonical example: swapping `http.DefaultTransport` for an instrumented one.
8. **`set_fields`** — set or wrap fields on struct literal constructions anywhere in the target package, for types with no constructor to hook.

**[Fact]** File-level predicates (`where.file`: `has_func`, `has_struct`, `has_directive`, `has_package`, `is_test`) and boolean combinators (`all-of`, `one-of`, `not`) compose the matchers.

**[Inference]** The eight rule types are not arbitrary — they are approximately the closure of "ways a Go program can be modified from outside." Rust's closure is different: no struct-literal-without-constructor problem to the same degree, but Rust adds trait impls, generic instantiations, and macro-generated code as categories Go does not have (§6).

### 2.7 Configuration and instrumentation selection

**[Fact]** ADR-0005 ("import-driven instrumentation selection") establishes that applications declare which instrumentations they want via blank imports in an `otel.instrumentation.go` (or `otelc.tool.go`) file next to `go.mod`:

```go
import (
	_ "go.opentelemetry.io/otelc/instrumentation/net/http/server"
	_ "go.opentelemetry.io/otelc/instrumentation/github.com/gin-gonic/gin"
)
```

This follows Go's established `tools.go` convention. **[Fact]** A package counts as an instrumentation package if it contains a tool file or one or more `*.otelc.yml` rule files, and instrumentation packages may import each other, giving composition.

**[Fact]** If no tool file exists, `otelc` auto-generates a temporary configuration from the dependency graph, preserving zero-configuration defaults. Rule source precedence, highest first: `OTELC_RULES` env var → `--rules` flag → tool file → default embedded rules.

**[Fact]** The rejected alternative was implicit matching against the dependency graph alone; it "did not scale to external instrumentations without imposing impractical maintenance burdens." The consequence of the chosen design is that third-party vendors can ship instrumentation as ordinary Go modules with independent versioning.

**[Inference — high relevance]** This is a governance/ecosystem decision as much as a technical one, and it is directly portable: a Rust tool should let instrumentation rules ship as ordinary crates, discovered from `Cargo.toml`, rather than baking a fixed rule set into the tool binary.

### 2.8 Dependency handling

**[Fact]** `path` fields in rules reference the package containing hook functions; that package must be available in the user's module at build time. With the tool-file mechanism, importing the instrumentation package adds the dependency automatically; with `--rules`/env vars, the package must already be in `go.mod`. For `inject_hooks`, "the tool automatically reads the hook source file and ensures all of its imports are present in the build."

### 2.9 Limitations

**[Fact]**
- Requires the ability to rebuild the application. This is the fundamental division of labour against eBPF.
- v1 covers five instrumentations, not the Go ecosystem.
- Persistent `otelc pin`-generated config files are still "under development"; committing them is not yet supported, so persistent configuration is currently a local-workflow-only feature.
- Requires Go 1.25+.

**[Fact — resolved in the verification pass]** The v1 blog claims "no added runtime overhead" but gives no numbers. `docs/benchmarking.md` was read directly during verification: it contains **no runtime overhead, binary-size, or timing figures of any kind**. Its only quantitative content is a CI gate — `BENCH_MAX_OVERHEAD_PCT=150`, failing a CI job when `otelc`'s *compile time* exceeds 150% of a plain `go build` baseline measured in the same run — and it documents *compile-time* benchmarking methodology (three scenarios: baseline, multi, largeidle), not runtime cost. So even the OTel-official, v1.1, production tool does not publish the runtime-overhead numbers its own marketing claims. We should not repeat that style of unsubstantiated claim about our own tool.

### 2.9a Real benchmark and compatibility data from the maintainers

**[Fact — from `otelc` maintainer Xabier Martinez, `#otel-go`; see [Appendix D.3](appendix-d-maintainer-qa.md).]** This partly corrects §2.9: the numbers do not live in `docs/benchmarking.md`, but they do exist, in CI via CodSpeed.

| Scenario | Plain `go build` | With `otelc` | Overhead |
| --- | --- | --- | --- |
| Baseline (single package) | 5.3 s | 19.9 s | **+275%** |
| Multi-package | 17.4 s | 26.8 s | **+54%** |

Two things this confirms and one it corrects:

- **Confirmed:** `otelc`'s published benchmarks measure **compile time only** (`BenchmarkCompile`). There is still no application runtime request-latency benchmark; per-library runtime benchmarking is deferred until instrumentation rules are decoupled into a separate repository. §2.9's warning against unsubstantiated runtime claims stands.
- **Corrected:** compile-time overhead *is* measured, and the figures are considerably worse than the `BENCH_MAX_OVERHEAD_PCT=150` gate would suggest for the single-package case — so that gate is evidently not applied uniformly across all three scenarios.
- **[Inference]** The two figures are the fixed-cost and marginal-cost ends of one curve: instrumentation setup is largely per-build, so it dominates a 5.3-second build and amortises across a 17.4-second one. **Our projection: 1.5×–3× clean compile time, worst on small projects** ([§14.3](14-evaluation-plan.md)).

**[Fact — automated forward-compatibility testing.]** `otelc` keeps rules working against upstream library releases with a scheduled CI workflow (`.github/workflows/test-latestlibrun.yaml`, tracked under issue #406) that fetches each instrumented library's latest stable release from the Go module proxy, runs the instrumentation tests against it, and **auto-files a tracking issue** (e.g. #565) when a private API changes and a rule's supported version range must be split.

**[Inference — adopt this, and adopt it early.]** This is the sixth concept to transfer (§2.11), and arguably the one with the longest half-life. Rules pinned to version ranges rot silently; the only sustainable answer at more than a handful of crates is a robot that notices. The Cargo port is direct: query the crates.io index for latest stable, run the instrumented build, open a tracking issue on breakage ([§14.6](14-evaluation-plan.md), [R16](13-technical-risks.md)).

### 2.10 What is Go-specific

| `otelc` design element | Go-specific? | Why |
| --- | --- | --- |
| `-toolexec` hook | **No** — Rust has `RUSTC_WRAPPER` | Both are "run my binary instead of the compiler" |
| Per-package compilation unit | **No** — Rust compiles per-crate | Rust's unit is coarser, which is *worse* for incremental rebuild cost |
| `//go:linkname` trampolines | **Yes** | Rust has no sanctioned equivalent; the nearest is `#[no_mangle] extern "C"` plus `-L`/`--extern` plumbing (§3.2.5) |
| Panic-catching trampoline | **Partially** | Rust has `catch_unwind`, but it is a no-op under `panic=abort` and is not free |
| Struct field injection | **Mostly yes** | Adding a field to a Rust struct changes its layout, its constructors, every exhaustive pattern match, and any `..Default::default()`. Far more invasive than in Go |
| Rewriting third-party source pre-compile | **No** | Directly portable |
| YAML rule schema with package/version/point selectors | **No** | Directly portable, and we should copy it closely |
| Import-driven instrumentation selection | **No** | Maps cleanly onto Cargo dependencies / features |
| Two-phase setup-then-instrument | **No** | Rust needs it for exactly the same reason |
| Auto-adding dependencies via `go mod tidy` | **Mostly no** | Rust's equivalent is editing `Cargo.toml`, but the lockfile and feature unification make this more delicate |
| Return-value mutation in hooks | **Yes** | Rust's ownership and move semantics make "hook rewrites the return value" much harder to do safely |

### 2.11 Concepts to transfer

**[Inference]** Five things we should take from `otelc`:

1. **Wrap the build tool; do not fork the compiler.** (`RUSTC_WRAPPER` ≈ `-toolexec`.)
2. **Two phases: resolve-and-prepare, then rewrite.** Injected code needs its dependencies to already exist.
3. **Declarative rules with package + version + point selectors.** Do not hardcode instrumentation into the tool.
4. **Instrumentation ships as ordinary library crates.** The tool is a mechanism; the rules are content.
5. **Start with the entry/exit function-hook rule type.** The other seven are refinements; `inject_hooks` alone gets you spans.
6. **[Added, [Appendix D.3](appendix-d-maintainer-qa.md)] Automate forward-compatibility testing against latest upstream releases** (the #406 pattern, §2.9a). Rules pinned to version ranges rot silently; a scheduled job that builds against latest stable and files a tracking issue on breakage is the only version of this that survives contact with a growing rule set.
---

---

← [Executive Summary](../../README.md) · [Contents](../../README.md) · [The Rust Compilation Pipeline](03-rust-compiler-pipeline.md) →
