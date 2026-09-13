#![cfg_attr(not(windows), allow(dead_code))]

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

const MARKER: &str = "/* __cargo_instrument_rust:p23 */";

/// This is process-level because Cargo must own its rustfix `RUSTC_WRAPPER`
/// proxy while the custom compiler occupies `RUSTC`.
#[test]
#[ignore = "requires nightly rustc-dev and the cached OpenTelemetry dependency"]
fn apply_round_trip_edits_only_owned_source_and_rebuilds_on_stable() {
    let fixture = Fixture::new();
    let original = fs::read_to_string(fixture.app_source()).unwrap();
    let dependency_before = fs::read_to_string(fixture.dependency_source()).unwrap();

    let first = fixture.run_apply();
    assert!(first.status.success(), "first apply failed:\n{first:?}");

    let edited = fs::read_to_string(fixture.app_source()).unwrap();
    assert_ne!(edited, original, "cargo fix output:\n{first:?}");
    assert_eq!(
        edited.matches(MARKER).count(),
        3,
        "edited source:\n{edited}"
    );
    assert_eq!(
        fs::read_to_string(fixture.dependency_source()).unwrap(),
        dependency_before
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
            "[workspace]\nmembers = [\"app\"]\nresolver = \"2\"\n",
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
            &temp.path().join("app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nexternal-dependency = { path = \"../external-dependency\" }\nopentelemetry = \"0.32.0\"\n",
        );
        write(
            &temp.path().join("app/src/lib.rs"),
            "pub fn sync(value: i32) -> i32 { external_dependency::plus_one(value) }\n\npub struct Service;\n\nimpl Service {\n    pub fn method(&self, value: i32) -> i32 { value * 2 }\n\n    pub async fn asynchronous(&self, value: i32) -> i32 { value + 3 }\n}\n",
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

    fn run_apply(&self) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_cargo-instrument-rust"))
            .args(["--apply", "--offline"])
            .current_dir(self.root())
            .output()
            .unwrap()
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
