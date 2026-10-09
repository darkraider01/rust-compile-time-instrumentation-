#!/usr/bin/env python3
"""
Phase 3: Repeat-Build Metadata and Pre-Pass Reuse Invalidation Experiments

This script executes four deterministic empirical experiments investigating the
invalidation boundary for Cargo metadata and artifact pre-pass reuse:

- Experiment A: External path dependency manifest changes without workspace changes.
- Experiment B: CLI flags and environment variables vs metadata resolution.
- Experiment C: Dynamic target file discovery (lib, main, bin, tests, benches, examples, build.rs).
- Experiment D: Source code edits and pre-pass recompilation vs metadata immutability.
"""

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

def sha256_file(path):
    if not os.path.exists(path):
        return None
    h = hashlib.sha256()
    with open(path, "rb") as f:
        h.update(f.read())
    return h.hexdigest()

def run_cmd(args, cwd, env=None):
    merged_env = os.environ.copy()
    if env:
        merged_env.update(env)
    res = subprocess.run(args, cwd=cwd, env=merged_env, capture_output=True, text=True)
    return res

def test_experiment_a():
    print("=== Experiment A: External Path Dependency Manifest Mutation ===")
    tmpdir = tempfile.mkdtemp(prefix="reuse_exp_a_")
    try:
        # External path dependency outside workspace
        ext_dir = os.path.join(tmpdir, "external_dep")
        os.makedirs(os.path.join(ext_dir, "src"), exist_ok=True)
        ext_manifest = os.path.join(ext_dir, "Cargo.toml")
        with open(ext_manifest, "w") as f:
            f.write("""[package]
name = "external_dep"
version = "0.1.0"
edition = "2021"

[features]
default = []
initial_feature = []

[dependencies]
""")
        with open(os.path.join(ext_dir, "src", "lib.rs"), "w") as f:
            f.write("pub fn ext_call() -> u32 { 100 }\n")

        # Workspace with member app
        ws_dir = os.path.join(tmpdir, "ws")
        app_dir = os.path.join(ws_dir, "app")
        os.makedirs(os.path.join(app_dir, "src"), exist_ok=True)
        ws_manifest = os.path.join(ws_dir, "Cargo.toml")
        with open(ws_manifest, "w") as f:
            f.write("""[workspace]
members = ["app"]
resolver = "2"
""")
        app_manifest = os.path.join(app_dir, "Cargo.toml")
        with open(app_manifest, "w") as f:
            f.write("""[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
external_dep = { path = "../../external_dep" }
""")
        with open(os.path.join(app_dir, "src", "main.rs"), "w") as f:
            f.write("fn main() { println!(\"{}\", external_dep::ext_call()); }\n")

        # Initial metadata query
        res1 = run_cmd(["cargo", "metadata", "--format-version", "1"], cwd=ws_dir)
        assert res1.returncode == 0, res1.stderr
        meta_before = json.loads(res1.stdout)
        pkg_before = next(p for p in meta_before["packages"] if p["name"] == "external_dep")
        features_before = set(pkg_before["features"].keys())

        # Record manifest hashes
        ws_hash_before = sha256_file(ws_manifest)
        app_hash_before = sha256_file(app_manifest)
        lock_path = os.path.join(ws_dir, "Cargo.lock")
        lock_hash_before = sha256_file(lock_path)

        # Mutate external dependency: add a new feature
        with open(ext_manifest, "w") as f:
            f.write("""[package]
name = "external_dep"
version = "0.1.0"
edition = "2021"

[features]
default = []
initial_feature = []
newly_added_feature = []

[dependencies]
""")

        # Verify workspace files are 100% UNCHANGED
        ws_hash_after = sha256_file(ws_manifest)
        app_hash_after = sha256_file(app_manifest)
        lock_hash_after = sha256_file(lock_path)
        assert ws_hash_before == ws_hash_after, "Workspace root Cargo.toml should not change"
        assert app_hash_before == app_hash_after, "Member app Cargo.toml should not change"
        assert lock_hash_before == lock_hash_after, "Workspace Cargo.lock should not change"

        # Re-query metadata
        res2 = run_cmd(["cargo", "metadata", "--format-version", "1"], cwd=ws_dir)
        assert res2.returncode == 0, res2.stderr
        meta_after = json.loads(res2.stdout)
        pkg_after = next(p for p in meta_after["packages"] if p["name"] == "external_dep")
        features_after = set(pkg_after["features"].keys())

        # Substantive assertions
        assert meta_before != meta_after, "Metadata should diverge when external path dep changes"
        assert "newly_added_feature" not in features_before
        assert "newly_added_feature" in features_after
        print("  [PASS] Workspace manifest and lockfile hashes are identical.")
        print("  [PASS] Cargo metadata resolved features diverged: 'newly_added_feature' discovered.")
        print("  [CONCLUSION] Manifest-only checking of workspace members produces false cache hits for path dependencies.")
    finally:
        shutil.rmtree(tmpdir)

def test_experiment_b():
    print("\n=== Experiment B: Metadata-Resolution Flags vs Compiler Environment ===")
    tmpdir = tempfile.mkdtemp(prefix="reuse_exp_b_")
    try:
        ws_dir = os.path.join(tmpdir, "pkg")
        os.makedirs(os.path.join(ws_dir, "src"), exist_ok=True)
        manifest = os.path.join(ws_dir, "Cargo.toml")
        with open(manifest, "w") as f:
            f.write("""[package]
name = "pkg"
version = "0.1.0"
edition = "2021"

[features]
default = ["std"]
std = []
extra = []
""")
        with open(os.path.join(ws_dir, "src", "main.rs"), "w") as f:
            f.write("fn main() {}\n")

        # 1. Default invocation
        res_default = run_cmd(["cargo", "metadata", "--format-version", "1"], cwd=ws_dir)
        meta_default = json.loads(res_default.stdout)
        node_default = next(n for n in meta_default["resolve"]["nodes"] if "pkg" in n["id"])
        features_default = set(node_default.get("features", []))

        # 2. Invocations with forwarded metadata flags: --features extra
        res_feat = run_cmd(["cargo", "metadata", "--format-version", "1", "--features", "extra"], cwd=ws_dir)
        meta_feat = json.loads(res_feat.stdout)
        node_feat = next(n for n in meta_feat["resolve"]["nodes"] if "pkg" in n["id"])
        features_feat = set(node_feat.get("features", []))

        # 3. Invocation with --no-default-features
        res_no_def = run_cmd(["cargo", "metadata", "--format-version", "1", "--no-default-features"], cwd=ws_dir)
        meta_no_def = json.loads(res_no_def.stdout)
        node_no_def = next(n for n in meta_no_def["resolve"]["nodes"] if "pkg" in n["id"])
        features_no_def = set(node_no_def.get("features", []))

        # 4. Invocations with RUSTFLAGS in environment
        res_rustflags = run_cmd(["cargo", "metadata", "--format-version", "1"], cwd=ws_dir, env={"RUSTFLAGS": "--cfg custom_cfg"})
        meta_rustflags = json.loads(res_rustflags.stdout)

        # Assertions
        assert features_default == {"default", "std"}
        assert features_feat == {"default", "std", "extra"}
        assert features_no_def == set()
        assert features_default != features_feat
        assert features_default != features_no_def

        # RUSTFLAGS does NOT affect cargo metadata resolution for basic manifests
        assert meta_default == meta_rustflags, "RUSTFLAGS in environment does not alter cargo metadata output"

        print(f"  [PASS] Default resolved features: {features_default}")
        print(f"  [PASS] With --features extra: {features_feat}")
        print(f"  [PASS] With --no-default-features: {features_no_def}")
        print("  [PASS] RUSTFLAGS confirmed to have no effect on cargo metadata resolution.")
        print("  [CONCLUSION] Metadata-resolution flags (--features, --no-default-features) directly modify metadata resolve nodes without filesystem changes.")
        print("               Compiler-only inputs (RUSTFLAGS, --target, --release) govern artifact compilation, not cargo metadata.")
    finally:
        shutil.rmtree(tmpdir)

def test_experiment_c():
    print("\n=== Experiment C: Dynamic Target Discovery Across File Conventions ===")
    tmpdir = tempfile.mkdtemp(prefix="reuse_exp_c_")
    try:
        ws_dir = os.path.join(tmpdir, "pkg")
        os.makedirs(os.path.join(ws_dir, "src"), exist_ok=True)
        manifest = os.path.join(ws_dir, "Cargo.toml")
        with open(manifest, "w") as f:
            f.write("""[package]
name = "pkg"
version = "0.1.0"
edition = "2021"
""")
        with open(os.path.join(ws_dir, "src", "lib.rs"), "w") as f:
            f.write("pub fn run() {}\n")

        manifest_hash_before = sha256_file(manifest)

        # Query before adding targets
        res_before = run_cmd(["cargo", "metadata", "--format-version", "1"], cwd=ws_dir)
        meta_before = json.loads(res_before.stdout)
        pkg_before = next(p for p in meta_before["packages"] if p["name"] == "pkg")
        target_tuples_before = [(t["name"], t["kind"]) for t in pkg_before["targets"]]

        # Add all standard target conventions without touching Cargo.toml
        # 1. Main binary
        with open(os.path.join(ws_dir, "src", "main.rs"), "w") as f:
            f.write("fn main() {}\n")
        # 2. Named binary in src/bin/
        os.makedirs(os.path.join(ws_dir, "src", "bin"), exist_ok=True)
        with open(os.path.join(ws_dir, "src", "bin", "tool.rs"), "w") as f:
            f.write("fn main() {}\n")
        # 3. Nested named binary
        os.makedirs(os.path.join(ws_dir, "src", "bin", "daemon"), exist_ok=True)
        with open(os.path.join(ws_dir, "src", "bin", "daemon", "main.rs"), "w") as f:
            f.write("fn main() {}\n")
        # 4. Integration test
        os.makedirs(os.path.join(ws_dir, "tests"), exist_ok=True)
        with open(os.path.join(ws_dir, "tests", "integ.rs"), "w") as f:
            f.write("#[test] fn t() {}\n")
        # 5. Nested integration test
        os.makedirs(os.path.join(ws_dir, "tests", "suite"), exist_ok=True)
        with open(os.path.join(ws_dir, "tests", "suite", "main.rs"), "w") as f:
            f.write("#[test] fn t() {}\n")
        # 6. Benchmark
        os.makedirs(os.path.join(ws_dir, "benches"), exist_ok=True)
        with open(os.path.join(ws_dir, "benches", "bench1.rs"), "w") as f:
            f.write("fn main() {}\n")
        # 7. Example
        os.makedirs(os.path.join(ws_dir, "examples"), exist_ok=True)
        with open(os.path.join(ws_dir, "examples", "ex1.rs"), "w") as f:
            f.write("fn main() {}\n")
        # 8. Custom build script
        with open(os.path.join(ws_dir, "build.rs"), "w") as f:
            f.write("fn main() {}\n")

        manifest_hash_after = sha256_file(manifest)
        assert manifest_hash_before == manifest_hash_after, "Cargo.toml must remain untouched"

        # Query after adding target files
        res_after = run_cmd(["cargo", "metadata", "--format-version", "1"], cwd=ws_dir)
        meta_after = json.loads(res_after.stdout)
        pkg_after = next(p for p in meta_after["packages"] if p["name"] == "pkg")
        target_tuples_after = [(t["name"], t["kind"]) for t in pkg_after["targets"]]

        # Assertions
        assert len(target_tuples_before) == 1
        assert ("pkg", ["lib"]) in target_tuples_before
        assert ("pkg", ["bin"]) in target_tuples_after
        assert ("tool", ["bin"]) in target_tuples_after
        assert ("daemon", ["bin"]) in target_tuples_after
        assert ("integ", ["test"]) in target_tuples_after
        assert ("suite", ["test"]) in target_tuples_after
        assert ("bench1", ["bench"]) in target_tuples_after
        assert ("ex1", ["example"]) in target_tuples_after
        assert ("build-script-build", ["custom-build"]) in target_tuples_after
        assert len(target_tuples_after) == 9

        print(f"  [PASS] Cargo.toml is 100% UNCHANGED.")
        print(f"  [PASS] Initial targets: {target_tuples_before}")
        print(f"  [PASS] Discovered targets: {target_tuples_after}")
        print("  [CONCLUSION] Target discovery dynamically adds targets based on filesystem layout, including nested directories and build.rs.")
    finally:
        shutil.rmtree(tmpdir)

def test_experiment_d():
    print("\n=== Experiment D: Source Edits vs Metadata Immutability & Pre-Pass Rebuild ===")
    tmpdir = tempfile.mkdtemp(prefix="reuse_exp_d_")
    try:
        ws_dir = os.path.join(tmpdir, "ws")
        app_dir = os.path.join(ws_dir, "app")
        helper_dir = os.path.join(ws_dir, "helper")
        os.makedirs(os.path.join(app_dir, "src"), exist_ok=True)
        os.makedirs(os.path.join(helper_dir, "src"), exist_ok=True)

        ws_manifest = os.path.join(ws_dir, "Cargo.toml")
        with open(ws_manifest, "w") as f:
            f.write("""[workspace]
members = ["app", "helper"]
resolver = "2"
""")
        with open(os.path.join(helper_dir, "Cargo.toml"), "w") as f:
            f.write("""[package]
name = "helper"
version = "0.1.0"
edition = "2021"
""")
        helper_src = os.path.join(helper_dir, "src", "lib.rs")
        with open(helper_src, "w") as f:
            f.write("pub fn helper_val() -> u32 { 10 }\n")

        with open(os.path.join(app_dir, "Cargo.toml"), "w") as f:
            f.write("""[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
helper = { path = "../helper" }
""")
        with open(os.path.join(app_dir, "src", "main.rs"), "w") as f:
            f.write("fn main() { println!(\"{}\", helper::helper_val()); }\n")

        target_dir = os.path.join(ws_dir, "target")

        # 1. Initial pre-pass build
        res_build_1 = run_cmd(["cargo", "build", "--target-dir", target_dir, "--message-format=json-render-diagnostics"], cwd=ws_dir)
        assert res_build_1.returncode == 0, res_build_1.stderr
        deps_dir = os.path.join(target_dir, "debug", "deps")
        rlibs_1 = [f for f in os.listdir(deps_dir) if f.startswith("libhelper-") and f.endswith(".rlib")]
        assert len(rlibs_1) == 1
        helper_rlib_path = os.path.join(deps_dir, rlibs_1[0])
        mtime_1 = os.path.getmtime(helper_rlib_path)

        # 2. Initial metadata capture
        res_meta_before = run_cmd(["cargo", "metadata", "--format-version", "1"], cwd=ws_dir)
        meta_before_str = res_meta_before.stdout
        meta_before = json.loads(meta_before_str)

        # 3. Repeat pre-pass build (untouched) -> should be fresh
        res_build_repeat = run_cmd(["cargo", "build", "--target-dir", target_dir, "--message-format=json-render-diagnostics"], cwd=ws_dir)
        repeat_artifacts = [json.loads(l) for l in res_build_repeat.stdout.splitlines() if l.strip().startswith("{") and json.loads(l).get("reason") == "compiler-artifact"]
        helper_repeat = next(a for a in repeat_artifacts if a["target"]["name"] == "helper")
        assert helper_repeat["fresh"] is True, "Repeat build without changes must report fresh: true"
        assert os.path.getmtime(helper_rlib_path) == mtime_1, "mtime must be unchanged"

        # 4. Mutate helper/src/lib.rs (source-only edit; manifests untouched)
        time.sleep(0.05)
        with open(helper_src, "w") as f:
            f.write("pub fn helper_val() -> u32 { 9999 }\n")

        # 5. Capture metadata after source edit
        res_meta_after = run_cmd(["cargo", "metadata", "--format-version", "1"], cwd=ws_dir)
        meta_after_str = res_meta_after.stdout
        meta_after = json.loads(meta_after_str)

        # Substantive assertions on metadata equality
        assert meta_before == meta_after, "Cargo metadata MUST be byte-for-byte identical after source-only edit"
        assert meta_before_str == meta_after_str, "Raw metadata JSON stdout must match exactly"

        # 6. Re-run pre-pass build to observe compilation
        res_build_after = run_cmd(["cargo", "build", "--target-dir", target_dir, "--message-format=json-render-diagnostics"], cwd=ws_dir)
        assert res_build_after.returncode == 0
        after_artifacts = [json.loads(l) for l in res_build_after.stdout.splitlines() if l.strip().startswith("{") and json.loads(l).get("reason") == "compiler-artifact"]
        helper_after = next(a for a in after_artifacts if a["target"]["name"] == "helper")

        # In Cargo, helper was dirty and was recompiled:
        assert helper_after["fresh"] is False, "Helper must be recompiled (fresh: false) after source edit"
        mtime_after = os.path.getmtime(helper_rlib_path)
        assert mtime_after > mtime_1, "Helper rlib mtime must advance upon recompilation"

        print("  [PASS] Metadata before and after source-only edit is 100% IDENTICAL.")
        print(f"  [PASS] Cargo pre-pass correctly detected dirty source: helper fresh={helper_after['fresh']}.")
        print(f"  [PASS] Helper rlib mtime advanced: {mtime_1} -> {mtime_after}.")
        print("  [CONCLUSION] Manifest and metadata checks are completely blind to source code modifications.")
        print("               Manifest-only validation is insufficient to determine whether pre-pass artifacts need recompilation.")
    finally:
        shutil.rmtree(tmpdir)

if __name__ == "__main__":
    print(f"=== Starting Phase 3 Invalidation Experiments ({time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}) ===")
    test_experiment_a()
    test_experiment_b()
    test_experiment_c()
    test_experiment_d()
    print("\n=== All 4 Experiments Passed Programmatic Assertions Cleanly ===")
