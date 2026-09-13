#![cfg_attr(not(windows), allow(dead_code))]

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

const MARKER: &str = "/* __cargo_instrument_rust:p23 */";

/// This is process-level because Cargo must own its rustfix `RUSTC_WRAPPER`
/// proxy while the custom compiler occupies `RUSTC`.
#[test]
#[ignore = "requires the pinned P2.3 nightly toolchain and rustc-dev"]
fn cargo_subcommand_apply_is_owned_idempotent_and_async_safe() {
    let fixture = Fixture::new();
    let original = fs::read_to_string(fixture.app_source()).unwrap();
    let dependency_before = fs::read_to_string(fixture.dependency_source()).unwrap();
    let nested_dependency_before = fs::read_to_string(fixture.nested_dependency_source()).unwrap();
    let outside_source_before = fs::read_to_string(fixture.outside_source()).unwrap();

    let first = fixture.run_apply();
    assert!(first.status.success(), "first apply failed:\n{first:?}");

    let edited = fs::read_to_string(fixture.app_source()).unwrap();
    assert_ne!(edited, original, "cargo fix output:\n{first:?}");
    assert_eq!(
        edited.matches(MARKER).count(),
        5,
        "edited source:\n{edited}"
    );
    assert!(
        edited.contains("(42)"),
        "unrelated Rustfix edit leaked:\n{edited}"
    );
    assert!(edited.contains("pub const fn constant() -> i32 { 7 }"));
    assert!(edited.contains("pub extern \"C\" fn exported() -> i32 { 9 }"));
    let asynchronous = edited
        .split("pub async fn asynchronous")
        .nth(1)
        .and_then(|source| {
            source
                .split("\n}\n}\n\npub fn assert_async_future_is_send")
                .next()
        })
        .expect("instrumented async method body");
    assert!(asynchronous.contains("FutureExt::with_context(async move"));
    assert!(asynchronous.contains("std::future::ready(()).await"));
    assert!(
        !asynchronous.contains("_cargo_instrument_rust_guard"),
        "an async context guard must not cross await:\n{asynchronous}"
    );
    assert_eq!(
        fs::read_to_string(fixture.dependency_source()).unwrap(),
        dependency_before
    );
    assert_eq!(
        fs::read_to_string(fixture.nested_dependency_source()).unwrap(),
        nested_dependency_before,
        "an unselected path dependency below the selected package root was edited"
    );
    assert_eq!(
        fs::read_to_string(fixture.outside_source()).unwrap(),
        outside_source_before,
        "source outside the selected package root was edited"
    );
    fixture.stable_check();

    // The production command refuses to edit over uncommitted user changes.
    let dirty_retry = fixture.run_apply();
    assert!(!dirty_retry.status.success());
    assert!(String::from_utf8_lossy(&dirty_retry.stderr).contains("dirty Git worktree"));

    fixture.commit("apply P2.3 instrumentation");
    let before_second_apply = fs::read_to_string(fixture.app_source()).unwrap();
    let second = fixture.run_apply();
    assert!(second.status.success(), "second apply failed:\n{second:?}");
    assert_eq!(
        fs::read_to_string(fixture.app_source()).unwrap(),
        before_second_apply
    );
}

struct Fixture {
    temp: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        write(
            &temp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\nexclude = [\"external-dependency\", \"app/vendor/nested_dep\"]\nresolver = \"2\"\n",
        );
        write(
            &temp.path().join(".cargo/config.toml"),
            "[build]\nrustflags = [\"--cfg\", \"p23_fixture_cfg\"]\n",
        );
        write(&temp.path().join(".gitignore"), "/target\n");
        write(
            &temp.path().join("external-dependency/Cargo.toml"),
            "[package]\nname = \"external-dependency\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write(
            &temp.path().join("external-dependency/src/lib.rs"),
            "pub fn plus_one(value: i32) -> i32 { value + 1 }\n",
        );
        write(
            &temp.path().join("app/vendor/nested_dep/Cargo.toml"),
            "[package]\nname = \"nested-dependency\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nopentelemetry = \"0.32.0\"\n",
        );
        write(
            &temp.path().join("app/vendor/nested_dep/src/lib.rs"),
            "pub fn nested(value: i32) -> i32 { value * 10 }\n",
        );
        write(
            &temp.path().join("shared/generated.rs"),
            "pub fn generated_outside_package() -> i32 { 11 }\n",
        );
        write(
            &temp.path().join("app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nexternal-dependency = { path = \"../external-dependency\" }\nnested-dependency = { path = \"vendor/nested_dep\" }\nopentelemetry = \"0.32.0\"\n",
        );
        write(
            &temp.path().join("app/src/lib.rs"),
            "#[cfg(not(p23_fixture_cfg))]\ncompile_error!(\"the fixture Cargo rustflags must remain active\");\n\n#[path = \"../../shared/generated.rs\"]\npub mod generated;\n\npub fn sync(value: i32) -> i32 { external_dependency::plus_one(value) + nested_dependency::nested(0) }\n\npub fn unrelated_fixable_warning() -> i32 { (42) }\n\npub const fn constant() -> i32 { 7 }\n\npub extern \"C\" fn exported() -> i32 { 9 }\n\npub struct Service;\n\nimpl Service {\n    pub fn method(&self, value: i32) -> i32 { value * 2 }\n\n    pub async fn asynchronous(&self, value: i32) -> i32 {\n        std::future::ready(()).await;\n        value + 3\n    }\n}\n\npub fn assert_async_future_is_send() {\n    fn require_send<T: Send>(_: T) {}\n    require_send(Service.asynchronous(1));\n}\n",
        );
        let fixture = Self { temp };
        fixture.run_git(&["init"]);
        fixture.run_git(&["add", "."]);
        fixture.commit("initial fixture");
        fixture
    }

    fn root(&self) -> &Path {
        self.temp.path()
    }

    fn app_source(&self) -> std::path::PathBuf {
        self.root().join("app/src/lib.rs")
    }

    fn dependency_source(&self) -> std::path::PathBuf {
        self.root().join("external-dependency/src/lib.rs")
    }

    fn nested_dependency_source(&self) -> std::path::PathBuf {
        self.root().join("app/vendor/nested_dep/src/lib.rs")
    }

    fn outside_source(&self) -> std::path::PathBuf {
        self.root().join("shared/generated.rs")
    }

    fn run_apply(&self) -> std::process::Output {
        let binary = Path::new(env!("CARGO_BIN_EXE_cargo-instrument-rust"));
        let mut paths = vec![binary.parent().unwrap().to_path_buf()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let mut command = Command::new("cargo");
        command
            .args([
                "instrument-rust",
                "--apply",
                "--package",
                "app",
                "--offline",
            ])
            .current_dir(self.root())
            .env("PATH", std::env::join_paths(paths).unwrap());
        // Local developer images may expose the pinned compiler under the
        // rolling `nightly` alias. CI deliberately leaves this unset.
        if let Ok(toolchain) = std::env::var("P23_TEST_TOOLCHAIN") {
            command.env("CARGO_INSTRUMENT_RUST_TOOLCHAIN", toolchain);
        }
        command.output().unwrap()
    }

    fn stable_check(&self) {
        let output = Command::new("cargo")
            .args(["+stable", "check", "--workspace", "--offline"])
            .current_dir(self.root())
            .output()
            .unwrap();
        assert!(output.status.success(), "stable check failed:\n{output:?}");
    }

    fn commit(&self, message: &str) {
        self.run_git(&["add", "."]);
        self.run_git(&[
            "-c",
            "user.name=P2.3 Fixture",
            "-c",
            "user.email=p23@example.invalid",
            "commit",
            "-m",
            message,
        ]);
    }

    fn run_git(&self, arguments: &[&str]) {
        let status = Command::new("git")
            .args(arguments)
            .current_dir(self.root())
            .status()
            .unwrap();
        assert!(status.success(), "git {arguments:?} failed");
    }
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}
