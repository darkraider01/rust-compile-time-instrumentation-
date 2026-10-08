#!/usr/bin/env bash
# Phase 3 repeat-build overhead profiling and stage attribution (2026-10-09)
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT_DIR"

OUT_DIR="${OUT_DIR:-target/phase3-repeat-profiling-rerun}"
LOG="$OUT_DIR/profile.log"
VERBOSE_LOG="$OUT_DIR/verbose-cargo.log"
PATCH_FILE="$ROOT_DIR/evidence/phase3-overhead-repeat-build-profiling-2026-10-09/profiling.patch"

if [ -e "$LOG" ]; then
  echo "Error: refusing to overwrite existing log $LOG" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"
{
  echo "START_UTC=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "LOADAVG_START=$(cat /proc/loadavg)"
} > "$LOG"

CLEAN_BIN="$ROOT_DIR/target/release/cargo-instrument"
PROFILED_BIN="$ROOT_DIR/target/release/cargo-instrument-profiled"

if [ ! -f "$PATCH_FILE" ]; then
  echo "Error: profiling patch not found at $PATCH_FILE" >&2
  exit 1
fi

# 1. Require unchanged tracked build inputs (cargo-instrument, instrument-semantics, Cargo.toml, Cargo.lock)
TRACKED_BUILD_INPUTS=(
  "$ROOT_DIR/cargo-instrument/"
  "$ROOT_DIR/instrument-semantics/"
  "$ROOT_DIR/Cargo.toml"
  "$ROOT_DIR/Cargo.lock"
)
if ! git diff-index --quiet HEAD -- "${TRACKED_BUILD_INPUTS[@]}"; then
  echo "Error: Tracked build inputs (cargo-instrument/, instrument-semantics/, Cargo.toml, Cargo.lock) have uncommitted (staged or unstaged) changes. Working tree must be clean." >&2
  exit 1
fi

# 2. Rebuild both binaries afresh each invocation outside measured sections
restore_clean_source() {
  if [ -f "$ROOT_DIR/cargo-instrument/src/main.rs.bak" ]; then
    mv -f "$ROOT_DIR/cargo-instrument/src/main.rs.bak" "$ROOT_DIR/cargo-instrument/src/main.rs"
    touch "$ROOT_DIR/cargo-instrument/src/main.rs"
    CARGO_NET_OFFLINE=true cargo build --release -p cargo-instrument --bin cargo-instrument >/dev/null 2>&1 || true
  fi
}
trap restore_clean_source EXIT INT TERM

# Build isolated profiled binary from clean source + patch
echo "Building isolated profiled binary from profiling.patch..."
cp "$ROOT_DIR/cargo-instrument/src/main.rs" "$ROOT_DIR/cargo-instrument/src/main.rs.bak"
git apply "$PATCH_FILE"
touch "$ROOT_DIR/cargo-instrument/src/main.rs"
CARGO_NET_OFFLINE=true cargo build --release -p cargo-instrument --bin cargo-instrument
cp -f "$ROOT_DIR/target/release/cargo-instrument" "$PROFILED_BIN"

# Restore clean source and build clean production binary
echo "Building clean production binary..."
mv -f "$ROOT_DIR/cargo-instrument/src/main.rs.bak" "$ROOT_DIR/cargo-instrument/src/main.rs"
touch "$ROOT_DIR/cargo-instrument/src/main.rs"
CARGO_NET_OFFLINE=true cargo build --release -p cargo-instrument --bin cargo-instrument
trap - EXIT INT TERM

# 3. Record hashes and build configuration
CLEAN_SHA=$(sha256sum "$CLEAN_BIN" | awk '{print $1}')
PROFILED_SHA=$(sha256sum "$PROFILED_BIN" | awk '{print $1}')
PATCH_SHA=$(sha256sum "$PATCH_FILE" | awk '{print $1}')
HEAD_REV=$(git rev-parse HEAD)

{
  echo "BUILD_CONFIGURATION:"
  echo "  HEAD:              $HEAD_REV"
  echo "  patch_sha256:      $PATCH_SHA"
  echo "  cargo_lock_sha256: $(sha256sum "$ROOT_DIR/Cargo.lock" | awk '{print $1}')"
  echo "  cargo_toml_sha256: $(sha256sum "$ROOT_DIR/Cargo.toml" | awk '{print $1}')"
  echo "  clean_bin:         $CLEAN_BIN (sha256: $CLEAN_SHA)"
  echo "  profiled_bin:      $PROFILED_BIN (sha256: $PROFILED_SHA)"
  echo "  rustc:             $(rustc --version)"
  echo "  cargo:             $(cargo --version)"
  echo ""
} >> "$LOG"

# 4. Assert binary probe presence
if grep -a -q -F "[STAGE_PROFILE]" "$CLEAN_BIN"; then
  echo "Error: clean binary $CLEAN_BIN unexpectedly contains [STAGE_PROFILE] probe!" >&2
  exit 1
fi
if ! grep -a -q -F "[STAGE_PROFILE]" "$PROFILED_BIN"; then
  echo "Error: profiled binary $PROFILED_BIN is missing [STAGE_PROFILE] probe!" >&2
  exit 1
fi

python3 - "$ROOT_DIR" "$OUT_DIR" "$CLEAN_BIN" "$PROFILED_BIN" "$VERBOSE_LOG" "$CLEAN_SHA" "$PROFILED_SHA" >> "$LOG" 2>&1 << 'EOF'
import sys
import os
import shutil
import time
import subprocess
import tempfile
import statistics
import re

root_dir = sys.argv[1]
out_dir = sys.argv[2]
clean_bin = sys.argv[3]
profiled_bin = sys.argv[4]
verbose_log_path = sys.argv[5]
clean_sha = sys.argv[6]
profiled_sha = sys.argv[7]

print("===============================================================================")
print(" CARGO-INSTRUMENT REPEAT-BUILD OVERHEAD PROFILING (2026-10-09)")
print("===============================================================================\n")
print(f"Clean binary:    {clean_bin} (sha256: {clean_sha})")
print(f"Profiled binary: {profiled_bin} (sha256: {profiled_sha})")

assert clean_sha != profiled_sha, "Clean and profiled binary SHA-256 hashes must differ!"

# Assert binary provenance via direct byte inspection
with open(clean_bin, "rb") as f:
    if b"[STAGE_PROFILE]" in f.read():
        raise RuntimeError(f"Clean binary provenance check failed: {clean_bin} contains [STAGE_PROFILE] probe!")
with open(profiled_bin, "rb") as f:
    if b"[STAGE_PROFILE]" not in f.read():
        raise RuntimeError(f"Profiled binary provenance check failed: {profiled_bin} is missing [STAGE_PROFILE] probe!")
print("Binary provenance verified: clean binary has no probe; profiled binary contains opt-in probe.\n")

os.makedirs(os.path.join(root_dir, "target"), exist_ok=True)
fixture_dir = tempfile.mkdtemp(prefix="phase3_profile_", dir=os.path.join(root_dir, "target"))
print(f"Fixture directory: {fixture_dir}")

import atexit
atexit.register(lambda: shutil.rmtree(fixture_dir, ignore_errors=True))

otel_shim_path = os.path.join(root_dir, "otel-shim").replace('\\', '/')

# Setup workspace
with open(os.path.join(fixture_dir, "Cargo.toml"), "w") as f:
    f.write("""[workspace]
members = ["bench_app"]
exclude = ["bench_dep"]
resolver = "2"
""")

# Setup bench_dep with 20 functions
dep_dir = os.path.join(fixture_dir, "bench_dep")
os.makedirs(os.path.join(dep_dir, "src"), exist_ok=True)
with open(os.path.join(dep_dir, "Cargo.toml"), "w") as f:
    f.write("""[package]
name = "bench_dep"
version = "0.1.0"
edition = "2021"
""")

dep_lib = ["// 20 functions in dependency\n"]
for i in range(20):
    dep_lib.append(f"#[inline(never)]\npub fn compute_step_{i}(x: u64) -> u64 {{ (x.wrapping_mul(6364136223846793005)).wrapping_add({i}) }}\n")
with open(os.path.join(dep_dir, "src", "lib.rs"), "w") as f:
    f.write("".join(dep_lib))

# Setup bench_app
app_dir = os.path.join(fixture_dir, "bench_app")
os.makedirs(os.path.join(app_dir, "src"), exist_ok=True)
with open(os.path.join(app_dir, "Cargo.toml"), "w") as f:
    f.write(f"""[package]
name = "bench_app"
version = "0.1.0"
edition = "2021"

[dependencies]
bench_dep = {{ path = "../bench_dep" }}
otel-shim = {{ path = "{otel_shim_path}" }}
opentelemetry = "0.32.0"
opentelemetry_sdk = "0.32.0"
""")

with open(os.path.join(app_dir, "src", "main.rs"), "w") as f:
    f.write("""fn main() {
    println!("app main");
}
""")

def sanitize_env():
    env = os.environ.copy()
    for var in [
        "CARGO_INSTRUMENT_ACTIVE",
        "CARGO_INSTRUMENT_DEPENDENCIES",
        "CARGO_INSTRUMENT_REGISTRY",
        "CARGO_INSTRUMENT_SESSION",
        "CARGO_INSTRUMENT_SESSION_ID",
        "CARGO_INSTRUMENT_WRAPPER_MODE",
        "CARGO_INSTRUMENT_SENTINEL_MODE",
        "CARGO_INSTRUMENT_NATIVE_OTEL",
        "CARGO_INSTRUMENT_PROFILE",
        "INSTRUMENT_DEBUG",
        "RUSTC_WRAPPER",
    ]:
        env.pop(var, None)
    env["CARGO_NET_OFFLINE"] = "true"
    return env

# Generate lockfile
env = sanitize_env()
subprocess.run(["cargo", "generate-lockfile"], cwd=fixture_dir, env=env, check=True)

target_base = os.path.join(fixture_dir, "target_base")
target_inst = os.path.join(fixture_dir, "target_inst")

print("\n--- Executing setup builds (outside measurement) ---")
# Baseline setup
subprocess.run(["cargo", "build", "--target-dir", target_base], cwd=fixture_dir, env=env, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
# Instrumented setup using clean production binary
subprocess.run([clean_bin, "--with-dependencies", "--", "build", "--target-dir", target_inst], cwd=fixture_dir, env=env, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
print("Setup complete.\n")

SAMPLES = 10

# =============================================================================
# 1. UNPROFILED REPEAT BUILDS (N=10 pairs, clean production binary, alternating order)
# =============================================================================
print(f"=== 1. UNPROFILED REPEAT BUILDS (N={SAMPLES} pairs, clean binary, alternating order) ===")
unprofiled_base = []
unprofiled_inst = []
unprofiled_deltas = []

for run in range(SAMPLES):
    baseline_first = (run % 2 == 0)
    order_str = "baseline_first" if baseline_first else "instrumented_first"

    base_cmd = ["cargo", "build", "--target-dir", target_base]
    inst_cmd = [clean_bin, "--with-dependencies", "--", "build", "--target-dir", target_inst]

    env_clean = sanitize_env()

    if baseline_first:
        t0 = time.perf_counter()
        subprocess.run(base_cmd, cwd=fixture_dir, env=env_clean, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        d_base = time.perf_counter() - t0

        t1 = time.perf_counter()
        subprocess.run(inst_cmd, cwd=fixture_dir, env=env_clean, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        d_inst = time.perf_counter() - t1
    else:
        t1 = time.perf_counter()
        subprocess.run(inst_cmd, cwd=fixture_dir, env=env_clean, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        d_inst = time.perf_counter() - t1

        t0 = time.perf_counter()
        subprocess.run(base_cmd, cwd=fixture_dir, env=env_clean, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        d_base = time.perf_counter() - t0

    unprofiled_base.append(d_base)
    unprofiled_inst.append(d_inst)
    delta_i = d_inst - d_base
    unprofiled_deltas.append(delta_i)
    print(f"UNPROFILED sample {run}: baseline_repeat={d_base*1000:.2f}ms instrumented_repeat={d_inst*1000:.2f}ms paired_delta={delta_i*1000:+.2f}ms order={order_str}")

base_med = statistics.median(unprofiled_base) * 1000
inst_med = statistics.median(unprofiled_inst) * 1000
diff_of_medians = inst_med - base_med
median_paired_delta = statistics.median(unprofiled_deltas) * 1000

print(f"\nUNPROFILED SUMMARY (Clean Production Binary):")
print(f"  Baseline repeat:       median={base_med:.2f}ms (min={min(unprofiled_base)*1000:.2f}ms, max={max(unprofiled_base)*1000:.2f}ms, spread={((max(unprofiled_base)-min(unprofiled_base))*1000):.2f}ms)")
print(f"  Instrumented repeat:   median={inst_med:.2f}ms (min={min(unprofiled_inst)*1000:.2f}ms, max={max(unprofiled_inst)*1000:.2f}ms, spread={((max(unprofiled_inst)-min(unprofiled_inst))*1000):.2f}ms)")
print(f"  Difference of medians: {diff_of_medians:+.2f}ms (+{diff_of_medians / base_med * 100.0:.1f}%)")
print(f"  Median paired delta:   {median_paired_delta:+.2f}ms (min={min(unprofiled_deltas)*1000:+.2f}ms, max={max(unprofiled_deltas)*1000:+.2f}ms)\n")

# =============================================================================
# 2. DORMANT PROBE COMPARISON (N=10 runs, profiled binary with CARGO_INSTRUMENT_PROFILE unset)
# =============================================================================
print(f"=== 2. INACTIVE PROBE TRACK (N={SAMPLES} runs, profiled binary, profile inactive) ===")
dormant_inst = []
for run in range(SAMPLES):
    inst_cmd = [profiled_bin, "--with-dependencies", "--", "build", "--target-dir", target_inst]
    env_clean = sanitize_env()
    t0 = time.perf_counter()
    subprocess.run(inst_cmd, cwd=fixture_dir, env=env_clean, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    d = time.perf_counter() - t0
    dormant_inst.append(d)

dormant_med = statistics.median(dormant_inst) * 1000
dormant_overhead = dormant_med - inst_med
print(f"  Inactive probe median: {dormant_med:.2f}ms (min={min(dormant_inst)*1000:.2f}ms, max={max(dormant_inst)*1000:.2f}ms)")
print(f"  Clean binary median:   {inst_med:.2f}ms")
print(f"  Observed difference:   {dormant_overhead:+.2f}ms ({dormant_overhead / inst_med * 100.0:+.2f}% across consecutive blocks)\n")

# =============================================================================
# 3. PROFILED REPEAT BUILDS (N=10 pairs, isolated probe binary, stage breakdown)
# =============================================================================
print(f"=== 3. PROFILED REPEAT BUILDS (N={SAMPLES} pairs, isolated probe binary, opt-in stage timing) ===")
stage_keys = [
    "cli_init",
    "metadata_cmd",
    "metadata_plan",
    "prepass_cmd",
    "prepass_analysis",
    "session_save",
    "wrapped_cargo_cmd",
    "post_build",
    "total_wall",
]
profiled_stages = {k: [] for k in stage_keys}
profiled_base = []
profiled_ext_inst = []

profile_pattern = re.compile(r"\[STAGE_PROFILE\]\s+cli_init=([0-9.]+)ms\s+metadata_cmd=([0-9.]+)ms\s+metadata_plan=([0-9.]+)ms\s+prepass_cmd=([0-9.]+)ms\s+prepass_analysis=([0-9.]+)ms\s+session_save=([0-9.]+)ms\s+wrapped_cargo_cmd=([0-9.]+)ms\s+post_build=([0-9.]+)ms\s+total_wall=([0-9.]+)ms")

for run in range(SAMPLES):
    baseline_first = (run % 2 == 0)
    order_str = "baseline_first" if baseline_first else "instrumented_first"

    env_clean = sanitize_env()
    env_prof = sanitize_env()
    env_prof["CARGO_INSTRUMENT_PROFILE"] = "1"

    base_cmd = ["cargo", "build", "--target-dir", target_base]
    inst_cmd = [profiled_bin, "--with-dependencies", "--", "build", "--target-dir", target_inst]

    if baseline_first:
        t0 = time.perf_counter()
        subprocess.run(base_cmd, cwd=fixture_dir, env=env_clean, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        d_base = time.perf_counter() - t0

        t1 = time.perf_counter()
        p = subprocess.run(inst_cmd, cwd=fixture_dir, env=env_prof, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        d_inst = time.perf_counter() - t1
    else:
        t1 = time.perf_counter()
        p = subprocess.run(inst_cmd, cwd=fixture_dir, env=env_prof, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        d_inst = time.perf_counter() - t1

        t0 = time.perf_counter()
        subprocess.run(base_cmd, cwd=fixture_dir, env=env_clean, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        d_base = time.perf_counter() - t0

    profiled_base.append(d_base)
    profiled_ext_inst.append(d_inst)

    match = profile_pattern.search(p.stderr)
    if not match:
        raise RuntimeError(f"Failed to find STAGE_PROFILE in stderr:\n{p.stderr}")

    vals = [float(x) for x in match.groups()]
    for k, v in zip(stage_keys, vals):
        profiled_stages[k].append(v)

    stage_line = " ".join(f"{k}={v:.2f}ms" for k, v in zip(stage_keys, vals))
    print(f"PROFILED sample {run}: external_wall={d_inst*1000:.2f}ms {stage_line} (baseline={d_base*1000:.2f}ms, order={order_str})")

print(f"\nPROFILED STAGES MEDIAN & ATTRIBUTION:")
stage_medians = {}
for k in stage_keys:
    med = statistics.median(profiled_stages[k])
    spread = max(profiled_stages[k]) - min(profiled_stages[k])
    stage_medians[k] = med
    print(f"  {k:20s}: median={med:6.2f}ms (min={min(profiled_stages[k]):6.2f}ms, max={max(profiled_stages[k]):6.2f}ms, spread={spread:6.2f}ms)")

prof_ext_med = statistics.median(profiled_ext_inst) * 1000
prof_int_total_med = stage_medians["total_wall"]
startup_dispatch_diff = prof_ext_med - prof_int_total_med
active_probe_overhead = prof_ext_med - inst_med

print(f"\nTIMING BOUNDARY & OBSERVED TRACK DIFFERENCES:")
print(f"  Clean binary external wall median:     {inst_med:.2f}ms")
print(f"  Profiled binary external wall median:  {prof_ext_med:.2f}ms")
print(f"  Profiled binary internal total_wall:   {prof_int_total_med:.2f}ms")
print(f"  External - internal duration delta:    {startup_dispatch_diff:.2f}ms (startup, dispatch, teardown, output capture)")
print(f"  Observed clean-vs-active difference:   {active_probe_overhead:+.2f}ms ({active_probe_overhead / inst_med * 100.0:+.2f}% across consecutive blocks)")

subprocesses_sum = stage_medians["metadata_cmd"] + stage_medians["prepass_cmd"] + stage_medians["wrapped_cargo_cmd"]
in_process_sum = stage_medians["cli_init"] + stage_medians["metadata_plan"] + stage_medians["prepass_analysis"] + stage_medians["session_save"] + stage_medians["post_build"]

print(f"\nSTAGE ATTRIBUTION SUMMARY (relative to internal total_wall {prof_int_total_med:.2f}ms):")
print(f"  1. cargo metadata command (subprocess):               {stage_medians['metadata_cmd']:6.2f}ms ({stage_medians['metadata_cmd']/prof_int_total_med*100:5.1f}%)")
print(f"  2. unwrapped cargo pre-pass build (subprocess):       {stage_medians['prepass_cmd']:6.2f}ms ({stage_medians['prepass_cmd']/prof_int_total_med*100:5.1f}%)")
print(f"  3. wrapped cargo final build (subprocess):           {stage_medians['wrapped_cargo_cmd']:6.2f}ms ({stage_medians['wrapped_cargo_cmd']/prof_int_total_med*100:5.1f}%)")
print(f"     -> Subtotal Subprocess Time:                      {subprocesses_sum:6.2f}ms ({subprocesses_sum/prof_int_total_med*100:5.1f}%)")
print(f"  4. In-process metadata JSON parse & plan:              {stage_medians['metadata_plan']:6.2f}ms ({stage_medians['metadata_plan']/prof_int_total_med*100:5.1f}%)")
print(f"  5. In-process pre-pass artifact analysis & stamps:     {stage_medians['prepass_analysis']:6.2f}ms ({stage_medians['prepass_analysis']/prof_int_total_med*100:5.1f}%)")
print(f"  6. In-process session plan serialization:              {stage_medians['session_save']:6.2f}ms ({stage_medians['session_save']/prof_int_total_med*100:5.1f}%)")
print(f"  7. In-process CLI init & argument handling:            {stage_medians['cli_init']:6.2f}ms ({stage_medians['cli_init']/prof_int_total_med*100:5.1f}%)")
print(f"  8. In-process post-build policy write:                 {stage_medians['post_build']:6.2f}ms ({stage_medians['post_build']/prof_int_total_med*100:5.1f}%)")
print(f"     -> Subtotal In-Process Time:                      {in_process_sum:6.2f}ms ({in_process_sum/prof_int_total_med*100:5.1f}%)")

# =============================================================================
# 4. COMPILER & CACHE ACTIVITY VERIFICATION
# =============================================================================
print(f"\n=== 4. COMPILER & CACHE ACTIVITY VERIFICATION ===")

# Capture artifact timestamps before repeat build
deps_dir = os.path.join(target_inst, "debug", "deps")
mtimes_before = {}
if os.path.isdir(deps_dir):
    for fname in os.listdir(deps_dir):
        fpath = os.path.join(deps_dir, fname)
        mtimes_before[fname] = os.path.getmtime(fpath)

# Capture verbose build output
p_verbose = subprocess.run([clean_bin, "--with-dependencies", "--", "build", "--target-dir", target_inst, "-vv"], cwd=fixture_dir, env=sanitize_env(), check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

# Preserve full verbose output in file
with open(verbose_log_path, "w") as vf:
    vf.write("=== STDOUT ===\n")
    vf.write(p_verbose.stdout)
    vf.write("\n=== STDERR ===\n")
    vf.write(p_verbose.stderr)

# Check artifact timestamps after repeat build
mtimes_after = {}
modified_artifacts = []
if os.path.isdir(deps_dir):
    for fname in os.listdir(deps_dir):
        fpath = os.path.join(deps_dir, fname)
        mtimes_after[fname] = os.path.getmtime(fpath)
        if mtimes_before.get(fname) != mtimes_after[fname]:
            modified_artifacts.append(fname)

# Count compiled vs fresh from verbose diagnostic stream
fresh_count = len(re.findall(r"^\s*Fresh\s+", p_verbose.stderr, re.MULTILINE))
compiling_count = len(re.findall(r"^\s*Compiling\s+", p_verbose.stderr, re.MULTILINE))

print(f"Verbose Cargo output analysis:")
print(f"  Verbose log preserved at:     {verbose_log_path}")
print(f"  Units reported 'Fresh':       {fresh_count}")
print(f"  Units reported 'Compiling':   {compiling_count}")
print(f"  Artifact files modified:     {len(modified_artifacts)}")
assert compiling_count == 0, f"Expected 0 units compiled on repeat build, found {compiling_count}!"
assert fresh_count > 0, f"Expected >0 units reported Fresh, found {fresh_count}!"
assert len(modified_artifacts) == 0, f"Expected 0 artifact modifications, found {len(modified_artifacts)}: {modified_artifacts}"
print("Verification passed: Cargo reports all units Fresh, 0 units Compiling, and 0 artifact modifications.\n")

# Cleanup temp fixture
shutil.rmtree(fixture_dir, ignore_errors=True)
EOF

{
  echo "LOADAVG_END=$(cat /proc/loadavg)"
  echo "END_UTC=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} >> "$LOG"

echo "Profiling execution completed successfully. Log: $LOG"
