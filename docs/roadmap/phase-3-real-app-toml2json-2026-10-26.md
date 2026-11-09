# Phase 3 Real-Application Evaluation — `toml2json` (2026-10-26)

[Project README](../../README.md) · [Phase 3 roadmap](phase-3-evaluation.md) · [Earlier repeat-build profile](phase-3-repeat-build-profiling-2026-10-09.md)

The corrected Python harness completed the full evaluation on 2026-10-26 from fresh, temporary application checkouts using the preserved lockfiles and a local Git object source. Phase 3 remains open. This is one synchronous CLI workload, without first-party source application, async services, or network exporting.

## Application and experiment

- Application: [woodruffw/toml2json](https://github.com/woodruffw/toml2json), revision `840c3286589c7430c0315949d73e875592edac60`, MIT.
- Instrumentation revision: `173b23b478c2a6fec03c14ec50f08b5a90979af2`. The harness rebuilds the tool outside measurement and records its binary hash and the executed revision.
- Workload: convert the preserved 140-byte TOML input to JSON. Runtime iterations parse a fixed two-argument command line, parse the already-read TOML, and serialize to a memory buffer. They exclude file reads, stdout, provider setup, and shutdown.
- Untouched: upstream source with a manifest-only `[workspace]` table to detach it from the enclosing tool workspace, plus the preserved upstream lockfile.
- A: the evaluation patch and evaluation lockfile, built normally.
- B: exactly the same source and lockfile as A, built through `cargo-instrument --with-dependencies -- build`.
- All measured builds use separate target directories and `--release --locked --offline`. Release LTO comes from the application's manifest.

The patch preserves upstream's `forbid(unsafe_code)` and stdout behavior. Its extra CLI flags, manual root span, in-memory exporter, counting processor, and validation loop are evaluation-only changes shared by A and B. Both SDK configurations select `AlwaysOn` explicitly. Timed runtime uses the synchronous atomic counting processor, without network export.

## Correctness and bounded coverage

The preflight and final binaries must produce identical stdout bytes to the untouched application. The expected SHA-256 for this input is `b6a2d0a2635019a99b56f47ba1d3d0b9425fc4b72e4e43437880adbe23b6826e`.

The initial B build must select native R-4 for `clap_lex`, `anstream`, and `anstyle_query`, without Tier-2 or first-party transformation. Full build stdout/stderr is retained in the command logs. These are selected-route checks, not an independently derived function coverage denominator.

Single-shot `--dump-spans` must export exactly one application root in A and that root plus 27 `clap_lex` spans in B. The new JSON dump includes each span's ID, trace ID, parent ID, name, and scope. The oracle checks unique nonzero IDs, a valid root, common trace identity, dependency scope, existing parents, absence of cycles, and termination of every chain at that root. It also checks three direct edges from the pinned `clap_lex` source:

- `RawArgs::new` → `<RawArgs as From<I>>::from`.
- `RawArgs::cursor` → `ArgCursor::new`.
- `RawArgs::next` → `ParsedArg<'s>::new`.

This does not constitute an independent census of every function or every expected edge. The serialization crates are executed but skipped by the syntactic `no_std` gate; other crates, including `clap_builder`, are skipped by unsafe-policy checks. Successful application execution does not imply instrumentation of conversion internals.

For each runtime sample, Rust checks argument-parsing success and compares every serialized output with the untouched output. This comparison and buffer consumption are inside both variants' timed loops. Python separately requires exact warm-up and measured invocation counts, output lengths, and span totals: 0 in A, 18 per iteration in B. Rust reads the measured counter before ending the manual root, so root spans are excluded explicitly. Warm-up is 500 iterations; measurement is 5,000. The evaluated application's `src/main.rs` and `Cargo.toml`, plus all three lockfiles, must retain their pre-build hashes. Registry source immutability is not separately proven by this harness.

## Corrected measurements

Five alternating build pairs and ten alternating runtime pairs, on one Linux host. Values are medians with [min, max]. Setup and tool bootstrap are outside measurement. Clean builds start with an empty variant target directory; this does not flush the OS filesystem cache. The source-touch case changes only the source mtime, rather than program content; it measures a triggered release rebuild, not Cargo's incremental compilation engine.

| Metric | A | B | Absolute difference | Difference |
| --- | --- | --- | --- | --- |
| Clean release build | 21.272 [20.714, 22.195] s | 35.522 [34.825, 37.551] s | +14.250 s | +66.99% |
| No-change release build | 0.085 [0.073, 0.100] s | 17.539 [15.774, 19.263] s | +17.454 s | +20516.17% |
| Source-mtime touch release build | 9.637 [9.138, 9.777] s | 23.894 [23.284, 25.372] s | +14.257 s | +147.94% |
| Runtime per iteration (descriptive; inconclusive) | 7277.456 [6258.482, 17975.341] ns | 13857.988 [11603.492, 24738.738] ns | +6580.532 ns | +90.42% |
| Release executable | 1,494,432 bytes | 1,565,848 bytes | +71,416 bytes | +4.78% |

Runtime timing is inconclusive: A spans 6.26–17.98 µs/iteration and B spans 11.60–24.74 µs/iteration. Both within-session ranges exceed the difference in medians. The table retains descriptive observations; it does not establish a stable cost per span. The untouched executable is 1,150,368 bytes. Its comparison with A includes telemetry, evaluation CLI flags, validation code, and exporter support, so it does not isolate telemetry setup size.

Host load, memory state, compiler versions, input hashes, commands, exit statuses, raw samples, and full per-command logs are under [`validated/`](../../evidence/phase3-real-app-toml2json-2026-10-26/validated/). The executed runner is archived as `validated/runner.py.gz`; its decompressed hash matches the manifest. The corrected loop and host conditions differ from the original session, so differences between sessions cannot be attributed to validation alone. One host/session and one small input do not establish application-wide performance or production readiness.

## No-change rebuild finding

The original instrumented no-change build median was 12.7143 seconds. A review reproduction took 13.142 seconds, removed 248 files (42.5 MiB), and recompiled dependencies and the application. The retained [review log](../../evidence/phase3-real-app-toml2json-2026-10-26/review-repeat-build.log.gz) and [artifact snapshot](../../evidence/phase3-real-app-toml2json-2026-10-26/review-repeat-build.json) record that observation.

This differs from the October 9 debug fixture's 31 Fresh units and zero compilations. Calling it an identical reproduction of that profile was incorrect. No-change means unchanged input; it does not mean the tool performed no compilation. The corrected no-change B samples report 23–25 `Compiling` lines each. The run preserves `Removed` and `Compiling` lines for every measured build, as well as the complete command logs.

Source inspection identifies a relevant invalidation path: `desired_instrumented_package_ids` selects reachable, non-excluded registry packages without applying the AST eligibility gates; `uninstrumented_fresh_package_ids` treats missing transformed stamps as uncertified; `acquire_native_artifacts` passes those packages to `invalidate_packages`. The wrapper deliberately writes transformed stamps only after successful mirrored compilation, leaving skipped crates unstamped. The post-build snapshot shows reachable, non-excluded `toml`, `serde_json`, and `strsim` artifacts without those stamps.

This is a source-supported explanation for repeated invalidation of skipped packages. It is not a complete causal attribution of every rebuilt unit or every millisecond. The exact invalidation set, Cargo dependency propagation, and certification behavior of instrumented units need a focused follow-up before selecting a production fix. The evaluation changes no cache policy, certification contract, or instrumentation architecture.

## Reproduce and inspect

Requirements: Git, Python 3.9+, stable Rust/Cargo compatible with the preserved lockfiles (recorded run: 1.99.0), a Linux host, and enough disk space under `target/`. Network access is needed for the initial repository clone and uncached Cargo packages. The timed phase is always offline.

From the tool checkout:

```sh
./evidence/phase3-real-app-toml2json-2026-10-26/run-eval.sh
```

The tracked runner clones the pinned application, applies `evaluation.patch`, installs `untouched.Cargo.lock` and `evaluation.Cargo.lock`, copies the input, fetches dependencies, rebuilds the tool, and runs all checks and measurements. Temporary application checkouts and artifacts are removed on success or failure. Full command logs remain with the results. It refuses to overwrite an existing `validated/commands.jsonl`; move the prior `validated/` directory to retain it before a new run.

To use an existing Git object source and cached Cargo dependencies, as in the corrected run:

```sh
./evidence/phase3-real-app-toml2json-2026-10-26/run-eval.sh \
  --source target/eval-workspace/toml2json-clean --offline
python3 -B -m unittest discover \
  -s evidence/phase3-real-app-toml2json-2026-10-26 -p test_eval.py -v
```

The existing clone is an optional object source, not a source of patched files, binaries, input, or lockfiles. Every run prepares fresh application directories from the pinned revision and tracked evidence.

## Validation and run disclosure

Nine regression tests passed, covering valid ancestry and exact counters plus rejection of missing parents, cross-trace spans, cycles, duplicate IDs, missing spans, and unexpected baseline spans. A compiled-binary negative probe rejected deliberately incorrect expected JSON (`negative-output-probe.json`). Python syntax, shell syntax, patched-source formatting, and workspace formatting checks passed.

The Python evaluation completed all build/runtime samples and final source/lockfile checks. Its original shell wrapper exited 2 afterward because the wrapper was edited while the shell was still reading it. That error is retained in `validated/session.log`. The corrected wrapper separately passed fresh-directory `--help` routing and refusal to overwrite existing results, including preservation of the prior session log; see `validated/wrapper-validation.json`. The full corrected shell wrapper was not rerun through another measurement session.

## Historical evidence and next work

The original `raw-results.json`, `env-manifest.txt`, and `variant-b-spans.txt` are preserved unchanged. The manifest's original patch hash refers to `historical-evaluation.patch`. Original runtime results lacked the new output/count checks; their 241 ns/span estimate remains historical, not a validated result of this harness. The original span dump lacked span/trace IDs and proved counts only, not ancestry.

The repeat-build invalidation causal investigation and stamp prefix defect fix on this application is completed and recorded in [phase-3-real-app-toml2json-rebuild-investigation-2026-11-05.md](phase-3-real-app-toml2json-rebuild-investigation-2026-11-05.md). First-party/combined instrumentation, real async services, exporter pipelines, wider input coverage, and matched manual comparisons remain deferred.
