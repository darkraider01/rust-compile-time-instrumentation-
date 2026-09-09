//! P2.1 regression suite: unit identity, instrumentation policy, and mirror isolation.
//!
//! Every test in this file reproduces a defect that is present on `main` as of
//! commit `409b774` (end of Phase 1). Each one asserts the *correct* Phase-2
//! behaviour, so it is red today and flips green when the corresponding P2.1 fix
//! lands. They exist to pin those defects in CI before any fix is attempted.
//!
//! ## Why these are integration tests
//!
//! All four defects live in the interaction between Cargo's build graph and
//! [`wrapper::run_wrapper`], not inside any single function:
//!
//! - the mirror path is derived from `--crate-name` alone
//!   (`wrapper.rs`, `mirror_and_transform_crate_sources`), which is **not** a unique
//!   compilation-unit key — Cargo reuses one crate name across package versions and
//!   across lib/test units;
//! - whether a spliced `extern "C"` trampoline will find a provider at link time is a
//!   property of the *final linked artifact*, which a single `rustc` invocation cannot
//!   observe at all.
//!
//! Neither is reachable from a synthetic unit test over `CrateInvocation::parse` or
//! `TransformationPlan`, so every test here drives real `cargo` and `rustc`
//! subprocesses over real fixture crates, in the style already established by
//! `cargo_integration_tests.rs` and `e2e_registry_tests.rs`.
//!
//! ## Running
//!
//! These are `#[ignore]`d so the default offline suite stays green — the same
//! convention `e2e_registry_tests.rs` uses for tests that cannot pass in the default
//! configuration. Run them explicitly:
//!
//! ```text
//! cargo test --test graph_topology_tests -- --ignored --nocapture
//! ```
//!
//! ## Offline by construction
//!
//! No fixture needs `CARGO_INSTRUMENT_REGISTRY`. Every instrumented crate is a local
//! path dependency, so the graph topology under test is produced entirely from
//! generated fixtures. The only registry resolution any fixture performs is for
//! `otel-shim`'s own `opentelemetry` dependency, which the workspace already builds.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

// ----------------------------------------------------------------------------
// Fixture helpers
// ----------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cargo-instrument parent is repo root")
        .to_path_buf()
}

/// Path to the in-repo `otel-shim` crate, escaped for embedding in a `Cargo.toml`.
fn otel_shim_dep_path() -> String {
    repo_root()
        .join("otel-shim")
        .to_string_lossy()
        .replace('\\', "/")
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture directory");
    }
    fs::write(path, contents).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

/// `<target-dir>/debug/deps/instrumented_sources` — the directory `wrapper.rs`
/// mirrors instrumented crate sources into, one subdirectory per `--crate-name`.
fn mirror_root(target_dir: &Path) -> PathBuf {
    target_dir
        .join("debug")
        .join("deps")
        .join("instrumented_sources")
}

/// Sorted names of every per-crate mirror directory produced by a build.
fn mirror_dirs(target_dir: &Path) -> Vec<String> {
    let root = mirror_root(target_dir);
    let Ok(entries) = fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

/// Mirror directories belonging to `crate_name`.
///
/// Matches both the current naming (`foo`) and any future unit-qualified naming
/// (`foo-<unit hash>`), so these tests assert the *number of distinct mirrors*
/// rather than hard-coding a directory layout the fix is expected to change.
fn mirror_dirs_for(target_dir: &Path, crate_name: &str) -> Vec<String> {
    mirror_dirs(target_dir)
        .into_iter()
        .filter(|d| d == crate_name || d.starts_with(&format!("{crate_name}-")))
        .collect()
}

/// Every `.rs` file underneath `dir`, as file names, sorted.
fn rs_file_names_under(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    collect_rs_files(dir, &mut out);
    out.sort();
    out
}

fn collect_rs_files(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path.file_name().unwrap().to_string_lossy().to_string());
        }
    }
}

/// Whether any mirrored source under `dir` had C-ABI trampoline calls spliced into it.
fn contains_trampoline_symbols(dir: &Path) -> bool {
    let mut files = Vec::new();
    collect_rs_paths(dir, &mut files);
    files.iter().any(|p| {
        fs::read_to_string(p)
            .map(|s| s.contains("__otel_span_enter"))
            .unwrap_or(false)
    })
}

fn collect_rs_paths(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_paths(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn describe(output: &Output) -> String {
    format!(
        "exit: {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

/// Run a Cargo subcommand against `manifest_dir`, optionally under the wrapper.
fn run_cargo(manifest_dir: &Path, target_dir: &Path, args: &[&str], instrumented: bool) -> Output {
    let mut cmd = Command::new("cargo");
    cmd.args(args)
        .arg("--target-dir")
        .arg(target_dir)
        .current_dir(manifest_dir)
        .env("CARGO_TERM_COLOR", "never");

    if instrumented {
        cmd.env("RUSTC_WRAPPER", env!("CARGO_BIN_EXE_cargo-instrument"))
            .env("INSTRUMENT_DEBUG", "1");
    }

    cmd.output().expect("failed to execute cargo")
}

/// Run cargo-instrument CLI entry point (`cargo-instrument -- build ...`).
fn run_cargo_instrument_cli(manifest_dir: &Path, target_dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cargo-instrument"));
    cmd.arg("--")
        .args(args)
        .arg("--target-dir")
        .arg(target_dir)
        .current_dir(manifest_dir)
        .env("CARGO_TERM_COLOR", "never")
        .env("INSTRUMENT_DEBUG", "1");

    cmd.output()
        .expect("failed to execute cargo-instrument CLI")
}

// ----------------------------------------------------------------------------
// Regression 1 — two versions of one package share a single mirror directory
// ----------------------------------------------------------------------------

/// **Defect (present on `409b774`):** the mirror directory is
/// `<out-dir>/instrumented_sources/<crate-name>`. `--out-dir` is the single shared
/// `deps` directory for every unit in a build, and `--crate-name` is `foo` for both
/// `foo v1.0.0` and `foo v2.0.0`. Both units therefore mirror into *one* directory.
///
/// Because mirroring only adds and overwrites files — it never removes them — the
/// shared directory ends up holding the union of both versions' sources, with files
/// common to both versions overwritten by whichever `rustc` process ran last. One of
/// the two `foo` units is then compiled from the *other* version's source.
///
/// **Observable, and why it is deterministic:** whichever version wins the race, the
/// program's output is wrong. `foo v1::shared` adds 1 and `foo v2::shared` adds 2, so
/// a correct build prints `2 3`; a collided build prints `3 3` or `2 2`, never `2 3`.
/// Comparing the instrumented run against an uninstrumented baseline of the same
/// fixture is therefore stable regardless of how the race resolves. The structural
/// assertions that follow it pin the mechanism rather than only the symptom.
///
/// Graph under test: `app -> foo v1` and `app -> bar -> foo v2`.
#[test]
#[ignore = "P2.1 regression: reproduces the multi-version mirror collision present on 409b774"]
fn test_multiple_versions_of_one_package_must_not_share_a_mirror() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    // foo v1.0.0 — `shared()` adds 1, and owns a module file no other version has.
    write_file(
        &root.join("foo_v1").join("Cargo.toml"),
        "[package]\nname = \"foo\"\nversion = \"1.0.0\"\nedition = \"2021\"\n\n[workspace]\n",
    );
    write_file(
        &root.join("foo_v1").join("src").join("lib.rs"),
        "mod only_v1;\n\npub fn shared(a: i32) -> i32 {\n    only_v1::helper(a)\n}\n",
    );
    write_file(
        &root.join("foo_v1").join("src").join("only_v1.rs"),
        "pub fn helper(a: i32) -> i32 {\n    a + 1\n}\n",
    );

    // foo v2.0.0 — `shared()` adds 2, with its own uniquely named module file.
    write_file(
        &root.join("foo_v2").join("Cargo.toml"),
        "[package]\nname = \"foo\"\nversion = \"2.0.0\"\nedition = \"2021\"\n\n[workspace]\n",
    );
    write_file(
        &root.join("foo_v2").join("src").join("lib.rs"),
        "mod only_v2;\n\npub fn shared(a: i32) -> i32 {\n    only_v2::helper(a)\n}\n",
    );
    write_file(
        &root.join("foo_v2").join("src").join("only_v2.rs"),
        "pub fn helper(a: i32) -> i32 {\n    a + 2\n}\n",
    );

    // bar depends on foo v2, pulling the second version into the graph transitively.
    write_file(
        &root.join("bar").join("Cargo.toml"),
        "[package]\nname = \"bar\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n\n\
         [dependencies]\nfoo = { path = \"../foo_v2\", version = \"2\" }\n",
    );
    write_file(
        &root.join("bar").join("src").join("lib.rs"),
        "pub fn call(a: i32) -> i32 {\n    foo::shared(a)\n}\n",
    );

    // The application links otel-shim so Tier-2 trampolines resolve; this test is
    // about mirror identity, not about link providers (see the no-provider test).
    let app_dir = root.join("app");
    write_file(
        &app_dir.join("Cargo.toml"),
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n\n\
             [dependencies]\nfoo = {{ path = \"../foo_v1\", version = \"1\" }}\n\
             bar = {{ path = \"../bar\" }}\n\
             otel-shim = {{ path = \"{}\" }}\n",
            otel_shim_dep_path()
        ),
    );
    write_file(
        &app_dir.join("src").join("main.rs"),
        "fn main() {\n    otel_shim::init();\n    \
         println!(\"{} {}\", foo::shared(1), bar::call(1));\n}\n",
    );

    // Baseline: the same fixture built without the wrapper, establishing ground truth.
    let plain_target = root.join("target").join("plain");
    let baseline = run_cargo(&app_dir, &plain_target, &["run", "--quiet"], false);
    assert!(
        baseline.status.success(),
        "uninstrumented baseline build must succeed (fixture sanity).\n{}",
        describe(&baseline)
    );
    let baseline_stdout = String::from_utf8_lossy(&baseline.stdout).trim().to_string();
    assert_eq!(
        baseline_stdout, "2 3",
        "fixture sanity: foo v1 must add 1 and foo v2 must add 2"
    );

    // Instrumented: same graph, through the wrapper.
    let instrumented_target = root.join("target").join("instrumented");
    let instrumented = run_cargo(&app_dir, &instrumented_target, &["run", "--quiet"], true);
    let instrumented_stdout = String::from_utf8_lossy(&instrumented.stdout)
        .trim()
        .to_string();

    // Primary assertion: instrumentation must not change program semantics.
    // Fails on 409b774 because one `foo` unit is compiled from the other version's source.
    assert_eq!(
        instrumented_stdout,
        baseline_stdout,
        "instrumented build must produce identical output to the uninstrumented baseline; \
         a differing result means one `foo` unit was compiled from the other version's \
         mirrored source.\n{}",
        describe(&instrumented)
    );

    // Mechanism assertion 1: two distinct packages require two distinct mirrors.
    let foo_mirrors = mirror_dirs_for(&instrumented_target, "foo");
    assert_eq!(
        foo_mirrors.len(),
        2,
        "each `foo` compilation unit must own a private mirror directory; found {:?} \
         among all mirrors {:?}",
        foo_mirrors,
        mirror_dirs(&instrumented_target)
    );

    // Mechanism assertion 2: no single mirror may hold sources from both versions.
    for dir_name in &foo_mirrors {
        let files = rs_file_names_under(&mirror_root(&instrumented_target).join(dir_name));
        let has_v1 = files.iter().any(|f| f == "only_v1.rs");
        let has_v2 = files.iter().any(|f| f == "only_v2.rs");
        assert!(
            !(has_v1 && has_v2),
            "mirror '{dir_name}' contains sources from BOTH foo v1 and foo v2 ({files:?}); \
             mirrored crate sources must never be merged across package versions"
        );
    }
}

// ----------------------------------------------------------------------------
// Regression 2 — host-side (proc-macro) dependencies receive Tier-2 trampolines
// ----------------------------------------------------------------------------

/// **Defect (present on `409b774`):** `wrapper.rs` selects `TrampolineEmitter` for any
/// crate whose role is not `Application`, which includes crates compiled for the *host*
/// as dependencies of a procedural macro. Those units are linked into the proc-macro
/// dynamic library, which does not link `otel-shim`, so the spliced
/// `__otel_span_enter` / `__otel_span_exit` symbols have no provider.
///
/// This is not something a single `rustc` invocation can detect: in a non-cross build a
/// host unit and a target unit of the same package are argv-identical apart from
/// `-C metadata`. It requires build-graph knowledge, which is why it is P2.1 scope.
///
/// **Observable:** the primary assertion is that no mirror is produced for the
/// host-side dependency at all — deterministic and identical on every platform. The
/// build-success assertion that follows is what actually breaks on MSVC
/// (`LNK2019: unresolved external symbol __otel_span_enter` → `LNK1120`); on ELF
/// targets a shared object may tolerate undefined symbols at link time, so that
/// assertion is the weaker of the two by design and the mirror check carries the test.
///
/// Graph under test: `app -> pmmacro (proc-macro) -> pmdep (ordinary lib)`.
#[test]
#[ignore = "P2.1 regression: reproduces host-side trampoline injection present on 409b774"]
fn test_proc_macro_host_dependency_must_not_be_instrumented() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    // An ordinary library that exists only to be used by a procedural macro at build
    // time. It is deliberately not `#![no_std]`, so it is not accidentally excluded by
    // the unrelated no_std filter that masks this defect for crates like `syn`.
    write_file(
        &root.join("pmdep").join("Cargo.toml"),
        "[package]\nname = \"pmdep\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    );
    write_file(
        &root.join("pmdep").join("src").join("lib.rs"),
        "pub fn shout(s: &str) -> String {\n    s.to_uppercase()\n}\n",
    );

    write_file(
        &root.join("pmmacro").join("Cargo.toml"),
        "[package]\nname = \"pmmacro\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n\n\
         [lib]\nproc-macro = true\n\n\
         [dependencies]\npmdep = { path = \"../pmdep\" }\n",
    );
    write_file(
        &root.join("pmmacro").join("src").join("lib.rs"),
        "use proc_macro::TokenStream;\n\n\
         #[proc_macro]\n\
         pub fn shout_lit(_input: TokenStream) -> TokenStream {\n    \
         let s = pmdep::shout(\"hello\");\n    \
         format!(\"{:?}\", s).parse().unwrap()\n}\n",
    );

    let app_dir = root.join("app");
    write_file(
        &app_dir.join("Cargo.toml"),
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n\n\
             [dependencies]\npmmacro = {{ path = \"../pmmacro\" }}\n\
             otel-shim = {{ path = \"{}\" }}\n",
            otel_shim_dep_path()
        ),
    );
    write_file(
        &app_dir.join("src").join("main.rs"),
        "fn main() {\n    otel_shim::init();\n    println!(\"{}\", pmmacro::shout_lit!());\n}\n",
    );

    let target_dir = root.join("target").join("instrumented");
    let output = run_cargo(&app_dir, &target_dir, &["build"], true);

    // Primary assertion (platform independent): a unit compiled for the host side of
    // the build must not be instrumented, because nothing on that side provides the
    // trampoline symbols. Checked before build status because mirroring happens
    // before `rustc` runs, so the evidence survives a failed build.
    let pmdep_mirrors = mirror_dirs_for(&target_dir, "pmdep");
    assert!(
        pmdep_mirrors.is_empty(),
        "host-side proc-macro dependency 'pmdep' must not be instrumented, but mirrors \
         {pmdep_mirrors:?} were produced; all mirrors: {:?}",
        mirror_dirs(&target_dir)
    );

    // Secondary assertion: with no host-side instrumentation, the build links cleanly.
    assert!(
        output.status.success(),
        "build of an application with a proc-macro dependency must succeed.\n{}",
        describe(&output)
    );
}

// ----------------------------------------------------------------------------
// Regression 3 — lib and test units of one package share a single mirror directory
// ----------------------------------------------------------------------------

/// **Defect (present on `409b774`):** Cargo compiles `src/lib.rs` twice when tests are
/// built — once as the library rlib and once as a `--test` harness — and both units
/// are passed the same `--crate-name`. Since the mirror path is keyed on the crate
/// name alone, both units mirror into the same directory and each may observe the
/// other's partially written files.
///
/// This is the same root cause as the multi-version collision but needs no third-party
/// or multi-version graph at all: it reproduces on an ordinary first-party crate with
/// an integration test, which makes it the cheapest proof that `--crate-name` is not a
/// compilation-unit identity.
///
/// **Observable:** the fixture declares `otel-shim`, so it is classified as an
/// application and instrumented with the sentinel emitter — no C-ABI symbols, hence no
/// link dependency, which isolates this test from the link-provider defect. The test
/// counts how many units the wrapper actually mirrored (from its own debug output) and
/// requires an equal number of distinct mirror directories.
#[test]
#[ignore = "P2.1 regression: reproduces the lib/test unit mirror collision present on 409b774"]
fn test_lib_and_test_units_must_not_share_a_mirror() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    let crate_name = "dual_unit_fixture";

    write_file(
        &root.join("Cargo.toml"),
        &format!(
            "[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [workspace]\n\n\
             [dependencies]\notel-shim = {{ path = \"{}\" }}\n",
            otel_shim_dep_path()
        ),
    );
    write_file(
        &root.join("src").join("lib.rs"),
        "pub fn setup() {\n    otel_shim::init();\n}\n\n\
         pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    );
    // The integration-test crate receives `--extern otel_shim` like any other target
    // in the package, so it must carry its own `otel_shim` item path to satisfy the
    // application preflight check in `ast::check_application_preflight`.
    write_file(
        &root.join("tests").join("it.rs"),
        &format!(
            "#[test]\nfn adds() {{\n    otel_shim::init();\n    {crate_name}::setup();\n    \
             assert_eq!({crate_name}::add(1, 2), 3);\n}}\n"
        ),
    );

    let target_dir = root.join("target").join("instrumented");
    let output = run_cargo(root, &target_dir, &["test", "--no-run"], true);
    assert!(
        output.status.success(),
        "building the test harness must succeed (fixture sanity).\n{}",
        describe(&output)
    );

    // How many compilation units did the wrapper actually mirror under this crate
    // name? Taken from the wrapper's own debug output so the expected mirror count is
    // derived from observed behaviour rather than hard-coded to Cargo's unit layout.
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mirrored_units = stderr
        .matches(&format!("crate={crate_name}] transformed"))
        .count();
    assert!(
        mirrored_units >= 2,
        "fixture sanity: Cargo must compile at least two units named '{crate_name}' \
         (library and test harness), but the wrapper mirrored {mirrored_units}.\n{}",
        describe(&output)
    );

    let mirrors = mirror_dirs_for(&target_dir, crate_name);
    assert_eq!(
        mirrors.len(),
        mirrored_units,
        "each compilation unit must own a private mirror directory: {mirrored_units} units \
         were mirrored but only {} directories exist ({mirrors:?})",
        mirrors.len()
    );
}

// ----------------------------------------------------------------------------
// Regression 4 — trampolines emitted into a graph with no otel-shim provider
// ----------------------------------------------------------------------------

/// **Defect (present on `409b774`):** `wrapper.rs` gates Tier-1 native OpenTelemetry
/// emission on the crate actually depending on `opentelemetry` (S11 fail-open), but
/// applies no equivalent gate to Tier-2. Any non-application crate is spliced with
/// `extern "C"` trampoline declarations regardless of whether `otel-shim` appears
/// anywhere in the build graph, so a graph with no provider fails at link time.
///
/// Unlike the proc-macro case, the failure here is in an *executable* link, which
/// requires every symbol to resolve on every supported platform — so this reproduces
/// identically on MSVC, ELF, and Mach-O.
///
/// **Observable:** the build must succeed. On `409b774` it fails with unresolved
/// `__otel_span_enter` / `__otel_span_exit`. The preceding assertion pins the cause:
/// no trampoline may be spliced into a graph that cannot provide one.
///
/// Graph under test: `app (bin) -> dep_lib`, with `otel-shim` absent entirely.
#[test]
#[ignore = "P2.1 regression: reproduces trampoline emission without a link provider on 409b774"]
fn test_no_shim_provider_must_not_emit_trampolines() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    write_file(
        &root.join("dep_lib").join("Cargo.toml"),
        "[package]\nname = \"dep_lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    );
    write_file(
        &root.join("dep_lib").join("src").join("lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    );

    let app_dir = root.join("app");
    write_file(
        &app_dir.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n\n\
         [dependencies]\ndep_lib = { path = \"../dep_lib\" }\n",
    );
    write_file(
        &app_dir.join("src").join("main.rs"),
        "fn main() {\n    println!(\"{}\", dep_lib::add(1, 2));\n}\n",
    );

    let target_dir = root.join("target").join("instrumented");
    let output = run_cargo(&app_dir, &target_dir, &["build"], true);

    // Cause: without a provider in the graph, no C-ABI trampoline may be spliced.
    // Checked first because mirroring precedes `rustc`, so this evidence survives the
    // link failure that follows from it.
    for dir_name in mirror_dirs_for(&target_dir, "dep_lib") {
        let dir = mirror_root(&target_dir).join(&dir_name);
        assert!(
            !contains_trampoline_symbols(&dir),
            "mirror '{dir_name}' had C-ABI trampolines spliced into it, but no crate in \
             this graph provides `__otel_span_enter`; instrumentation must fail open \
             instead (S11)"
        );
    }

    // Effect: the build must still succeed when no provider is available.
    assert!(
        output.status.success(),
        "a graph with no otel-shim provider must build successfully with instrumentation \
         skipped, but the build failed.\n{}",
        describe(&output)
    );
}

// ----------------------------------------------------------------------------
// D1 — Session cache fingerprint invalidation across Cargo.toml edits
// ----------------------------------------------------------------------------

/// **Defect (D1):** Previously, SessionPlan relied only on an arbitrary 1800s time
/// window. If `Cargo.toml` was edited to add or remove `otel-shim`, a warm cache
/// would reuse the stale plan:
/// - Removing `otel-shim`: warm cache retains `has_otel_shim_provider: true`, causing
///   trampolines to be spliced into non-application crates with no provider at link time (loud failure).
/// - Adding `otel-shim`: warm cache retains `false`, causing instrumentation to be silently skipped.
///
/// **Fix:** Fingerprinting over `Cargo.lock` + reachable manifests. Cache is rejected
/// as stale when manifests change.
#[test]
#[ignore = "P2.1 closeout: asserts cache invalidation on Cargo.toml edits in both directions"]
fn test_d1_session_cache_fingerprint_invalidation_both_directions() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    let dep_dir = root.join("dep_lib");
    write_file(
        &dep_dir.join("Cargo.toml"),
        "[package]\nname = \"dep_lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    );
    write_file(
        &dep_dir.join("src").join("lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    );

    let app_dir = root.join("app");
    // Step 1: Initial build with otel-shim present
    write_file(
        &app_dir.join("Cargo.toml"),
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n\n\
             [dependencies]\ndep_lib = {{ path = \"../dep_lib\" }}\n\
             otel-shim = {{ path = \"{}\" }}\n",
            otel_shim_dep_path()
        ),
    );
    write_file(
        &app_dir.join("src").join("main.rs"),
        "fn main() {\n    otel_shim::init();\n    println!(\"{}\", dep_lib::add(1, 2));\n}\n",
    );

    let target_dir = root.join("target").join("instrumented");
    let out1 = run_cargo_instrument_cli(&app_dir, &target_dir, &["build"]);
    assert!(
        out1.status.success(),
        "Step 1 build must succeed.\n{}",
        describe(&out1)
    );

    let session_path = target_dir.join("cargo_instrument_session.json");
    assert!(session_path.exists(), "Session cache must be created");
    let session_content = fs::read_to_string(&session_path).unwrap();
    assert!(session_content.contains("\"has_otel_shim_provider\": true"));

    // Direction 1 (Loud): Remove otel-shim and touch dep_lib
    write_file(
        &app_dir.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n\n\
         [dependencies]\ndep_lib = { path = \"../dep_lib\" }\n",
    );
    write_file(
        &app_dir.join("src").join("main.rs"),
        "fn main() {\n    println!(\"{}\", dep_lib::add(1, 2));\n}\n",
    );
    // Touch dep_lib so Cargo recompiles it within the same target dir
    write_file(
        &dep_dir.join("src").join("lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\n",
    );

    // Clean mirror directory from Step 1 to isolate Step 2's mirror creation
    let mirror_path = mirror_root(&target_dir);
    if mirror_path.exists() {
        let _ = fs::remove_dir_all(&mirror_path);
    }

    let out2 = run_cargo_instrument_cli(&app_dir, &target_dir, &["build"]);
    assert!(
        out2.status.success(),
        "Step 2 rebuild with otel-shim removed must succeed (cache must invalidate).\n{}",
        describe(&out2)
    );
    let session_content2 = fs::read_to_string(&session_path).unwrap();
    assert!(session_content2.contains("\"has_otel_shim_provider\": false"));

    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    assert!(
        stderr2.contains("no otel-shim provider found in build graph for 'dep_lib'"),
        "Step 2 must emit S11 fail-open warning for dep_lib when otel-shim is absent.\n{}",
        describe(&out2)
    );

    // Verify dep_lib was not mirrored
    assert!(
        mirror_dirs_for(&target_dir, "dep_lib").is_empty(),
        "dep_lib must not be mirrored after otel-shim was removed"
    );

    // Direction 2 (Silent): Add otel-shim back and touch dep_lib
    write_file(
        &app_dir.join("Cargo.toml"),
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n\n\
             [dependencies]\ndep_lib = {{ path = \"../dep_lib\" }}\n\
             otel-shim = {{ path = \"{}\" }}\n",
            otel_shim_dep_path()
        ),
    );
    write_file(
        &app_dir.join("src").join("main.rs"),
        "fn main() {\n    otel_shim::init();\n    println!(\"{}\", dep_lib::add(1, 2));\n}\n",
    );
    write_file(
        &dep_dir.join("src").join("lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\n\n",
    );

    let out3 = run_cargo_instrument_cli(&app_dir, &target_dir, &["build"]);
    assert!(
        out3.status.success(),
        "Step 3 rebuild with otel-shim restored must succeed.\n{}",
        describe(&out3)
    );
    let session_content3 = fs::read_to_string(&session_path).unwrap();
    assert!(session_content3.contains("\"has_otel_shim_provider\": true"));

    // Verify dep_lib mirror DOES receive trampolines
    let dep_mirrors = mirror_dirs_for(&target_dir, "dep_lib");
    assert!(!dep_mirrors.is_empty(), "dep_lib must have mirror");
    let has_trampoline = dep_mirrors.iter().any(|d| {
        let dir = mirror_root(&target_dir).join(d);
        contains_trampoline_symbols(&dir)
    });
    assert!(
        has_trampoline,
        "dep_lib must receive trampolines when otel-shim is present"
    );
}

// ----------------------------------------------------------------------------
// D2 — Hyphenated proc-macro package name normalization
// ----------------------------------------------------------------------------

/// **Defect (D2):** Cargo metadata uses hyphenated package names (`pm-dep`), but rustc
/// passes underscored crate names (`pm_dep`). Name matching must normalize hyphens to
/// underscores so host-only dependencies are recognized by name.
#[test]
#[ignore = "P2.1 closeout: asserts hyphenated host-only proc-macro dependency is excluded"]
fn test_d2_hyphenated_proc_macro_host_dependency() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    // Crate package name has a hyphen: "pm-dep"
    write_file(
        &root.join("pm-dep").join("Cargo.toml"),
        "[package]\nname = \"pm-dep\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    );
    write_file(
        &root.join("pm-dep").join("src").join("lib.rs"),
        "pub fn greet() -> &'static str { \"hello\" }\n",
    );

    write_file(
        &root.join("pm-macro").join("Cargo.toml"),
        "[package]\nname = \"pm-macro\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n\n\
         [lib]\nproc-macro = true\n\n\
         [dependencies]\npm-dep = { path = \"../pm-dep\" }\n",
    );
    write_file(
        &root.join("pm-macro").join("src").join("lib.rs"),
        "use proc_macro::TokenStream;\n\n\
         #[proc_macro]\n\
         pub fn emit_greet(_input: TokenStream) -> TokenStream {\n    \
         let s = pm_dep::greet();\n    \
         format!(\"{:?}\", s).parse().unwrap()\n}\n",
    );

    let app_dir = root.join("app");
    write_file(
        &app_dir.join("Cargo.toml"),
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n\n\
             [dependencies]\npm-macro = {{ path = \"../pm-macro\" }}\n\
             otel-shim = {{ path = \"{}\" }}\n",
            otel_shim_dep_path()
        ),
    );
    write_file(
        &app_dir.join("src").join("main.rs"),
        "fn main() {\n    otel_shim::init();\n    println!(\"{}\", pm_macro::emit_greet!());\n}\n",
    );

    let target_dir = root.join("target").join("instrumented");
    let output = run_cargo(&app_dir, &target_dir, &["build"], true);

    // Host-only package "pm-dep" (compiled as crate "pm_dep") must not have mirrors
    let mirrors = mirror_dirs_for(&target_dir, "pm_dep");
    assert!(
        mirrors.is_empty(),
        "hyphenated host-side dependency 'pm-dep' must not be instrumented; mirrors found: {mirrors:?}"
    );

    assert!(
        output.status.success(),
        "build with hyphenated proc-macro dependency must succeed.\n{}",
        describe(&output)
    );
}

// ----------------------------------------------------------------------------
// D3 — CLI exports precomputed SessionPlan, bypassing manifest search heuristic
// ----------------------------------------------------------------------------

/// **Defect (D3):** `find_best_manifest_dir` is a heuristic that can pick an unrelated
/// sibling directory if that sibling contains more scoring keywords. The CLI entry
/// point precomputes `SessionPlan` in `current_dir()` and exports it via
/// `CARGO_INSTRUMENT_SESSION`, guaranteeing wrapper child processes use the correct graph.
#[test]
#[ignore = "P2.1 closeout: asserts CLI precomputes plan from current directory rather than heuristic decoy"]
fn test_d3_cli_session_plan_avoids_wrong_manifest_heuristic() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    // Sibling decoy directory with otel-shim that scores highly on keywords
    let decoy_dir = root.join("decoy");
    write_file(
        &decoy_dir.join("Cargo.toml"),
        &format!(
            "[package]\nname = \"decoy\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n\n\
             [dependencies]\notel-shim = {{ path = \"{}\" }}\n",
            otel_shim_dep_path()
        ),
    );
    write_file(&decoy_dir.join("src").join("main.rs"), "fn main() {}\n");

    // Real target app being built: has NO otel-shim
    let app_dir = root.join("actual_app");
    write_file(
        &app_dir.join("Cargo.toml"),
        "[package]\nname = \"actual_app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    );
    write_file(
        &app_dir.join("src").join("main.rs"),
        "fn main() { println!(\"hello\"); }\n",
    );

    let target_dir = root.join("target").join("instrumented");
    let output = run_cargo_instrument_cli(&app_dir, &target_dir, &["build"]);
    assert!(
        output.status.success(),
        "CLI build must succeed.\n{}",
        describe(&output)
    );

    // The generated session plan must reflect `actual_app` (has_otel_shim_provider: false),
    // not the decoy sibling.
    let session_path = target_dir.join("cargo_instrument_session.json");
    assert!(session_path.exists(), "Session cache must exist");
    let session_content = fs::read_to_string(&session_path).unwrap();
    assert!(
        session_content.contains("\"has_otel_shim_provider\": false"),
        "Session plan must be computed from actual_app (no otel-shim), not the decoy sibling!\n{session_content}"
    );
}

/// **Defect (D3 follow-up):** CLI entry point must parse `--manifest-path` so that
/// `SessionPlan` is computed for the target workspace being built, not the directory
/// from which `cargo-instrument` was invoked.
#[test]
#[ignore = "P2.1 closeout: asserts CLI precomputes plan respecting --manifest-path"]
fn test_d3_cli_session_plan_respects_manifest_path_flag() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    // Decoy crate with NO otel-shim dependency (invocation cwd)
    let decoy_dir = root.join("decoy");
    write_file(
        &decoy_dir.join("Cargo.toml"),
        "[package]\nname = \"decoy\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    );
    write_file(&decoy_dir.join("src").join("main.rs"), "fn main() {}\n");

    // Real dependency library: dep_lib (has code to instrument)
    let dep_dir = root.join("real").join("dep_lib");
    write_file(
        &dep_dir.join("Cargo.toml"),
        "[package]\nname = \"dep_lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write_file(
        &dep_dir.join("src").join("lib.rs"),
        "pub fn compute() -> i32 { 42 }\n",
    );

    // Real target app: depends on dep_lib AND otel-shim
    let app_dir = root.join("real").join("app");
    let dep_lib_escaped = dep_dir.to_string_lossy().replace('\\', "/");
    write_file(
        &app_dir.join("Cargo.toml"),
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n\n\
             [dependencies]\ndep_lib = {{ path = \"{}\" }}\notel-shim = {{ path = \"{}\" }}\n",
            dep_lib_escaped,
            otel_shim_dep_path()
        ),
    );
    write_file(
        &app_dir.join("src").join("main.rs"),
        "fn main() {\n    otel_shim::init();\n    println!(\"{}\", dep_lib::compute());\n}\n",
    );

    let target_dir = root.join("target").join("instrumented");
    let app_manifest = app_dir.join("Cargo.toml");
    let app_manifest_str = app_manifest.to_string_lossy().to_string();

    let output = run_cargo_instrument_cli(
        &decoy_dir,
        &target_dir,
        &["build", "--manifest-path", &app_manifest_str],
    );
    assert!(
        output.status.success(),
        "CLI build must succeed.\n{}",
        describe(&output)
    );

    // 1. Session plan must have has_otel_shim_provider = true
    let session_path = target_dir.join("cargo_instrument_session.json");
    assert!(session_path.exists(), "Session cache must exist");
    let session_content = fs::read_to_string(&session_path).unwrap();
    assert!(
        session_content.contains("\"has_otel_shim_provider\": true"),
        "Session plan must be computed from real/app (has otel-shim), not the decoy cwd!\n{session_content}"
    );

    // 2. dep_lib mirror directory must contain spliced __otel_span_enter calls
    let dep_mirrors = mirror_dirs_for(&target_dir, "dep_lib");
    assert!(!dep_mirrors.is_empty(), "dep_lib must have mirror");
    let has_trampoline = dep_mirrors.iter().any(|d| {
        let dir = mirror_root(&target_dir).join(d);
        contains_trampoline_symbols(&dir)
    });
    assert!(
        has_trampoline,
        "dep_lib mirror must receive spliced __otel_span_enter calls when built via --manifest-path"
    );
}

// ----------------------------------------------------------------------------
// D5 — Unsafe default when cargo metadata fails
// ----------------------------------------------------------------------------

/// **Defect (D5):** SessionPlan::default() previously defaulted `has_otel_shim_provider`
/// to true, which on metadata failure would splice trampolines into dependencies and cause
/// link errors. It must default to false (fail-open per S11).
#[test]
#[ignore = "P2.1 closeout: asserts metadata failure defaults to no-shim provider per S11 fail-open"]
fn test_d5_metadata_failure_safe_default() {
    use cargo_instrument::SessionPlan;

    let plan = SessionPlan::default();
    assert!(
        !plan.has_otel_shim_provider(),
        "Default plan must have has_otel_shim_provider = false per S11 fail-open"
    );
}
