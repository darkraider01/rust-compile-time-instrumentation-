#!/usr/bin/env python3
import argparse
from datetime import datetime, timezone
import gzip
import hashlib
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import tempfile
import time

EVIDENCE_DIR = Path(__file__).resolve().parent
ROOT_DIR = EVIDENCE_DIR.parent.parent
APP_REPO = "https://github.com/woodruffw/toml2json"
APP_REVISION = "840c3286589c7430c0315949d73e875592edac60"
OUTPUT_DIR = EVIDENCE_DIR / "validated"
EVAL_DIR = ROOT_DIR
COMMAND_NUMBER = 0


def get_sanitized_env():
    env = os.environ.copy()
    for key in list(env):
        if key.startswith(("CARGO_INSTRUMENT_", "__CARGO_INSTRUMENT_FAULT_INJECT_", "OTEL_", "CARGO_PROFILE_")) or key in (
            "INSTRUMENT_DEBUG", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
            "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_TARGET_DIR",
        ):
            del env[key]
    return env


def save_json(path, value):
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run_cmd(cmd, cwd=None, extra_env=None):
    global COMMAND_NUMBER
    env = get_sanitized_env()
    env.update(extra_env or {})
    cmd = [str(arg) for arg in cmd]
    cwd = Path(cwd or EVAL_DIR)
    start = time.perf_counter()
    result = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True)
    elapsed = time.perf_counter() - start
    COMMAND_NUMBER += 1
    log = f"command-{COMMAND_NUMBER:03d}.log.gz"
    with gzip.open(OUTPUT_DIR / log, "wb") as stream:
        stream.write(result.stdout + b"\n--- stderr ---\n" + result.stderr)
    with (OUTPUT_DIR / "commands.jsonl").open("a") as stream:
        stream.write(json.dumps({"command": cmd, "cwd": str(cwd), "seconds": elapsed,
                                 "exit_status": result.returncode, "log": log,
                                 "environment_overrides": extra_env or {}}) + "\n")
    if result.returncode:
        raise RuntimeError(f"Command failed ({result.returncode}): {cmd}\n{result.stderr.decode(errors='replace')}")
    return elapsed, result.stdout, result.stderr.decode()


def compute_stats(values):
    return {"samples": values, "median": statistics.median(values),
            "min": min(values), "max": max(values), "spread": max(values) - min(values)}


def verify_trace(spans, dependency_count):
    roots = [span for span in spans if span["name"] == "app_root" and span["scope"] == "toml2json"]
    if len(roots) != 1 or len(spans) != dependency_count + 1:
        raise ValueError("Unexpected root or dependency span count")
    root = roots[0]
    by_id = {span["span_id"]: span for span in spans}
    if len(by_id) != len(spans) or "0" * 16 in by_id or root["trace_id"] == "0" * 32:
        raise ValueError("Invalid or duplicate span IDs")
    if root["parent_span_id"] != "0" * 16:
        raise ValueError("Application root has a parent")
    for span in spans:
        if span["trace_id"] != root["trace_id"]:
            raise ValueError("Span escaped the application trace")
        if span is not root and span["scope"] != "clap_lex":
            raise ValueError("Unexpected dependency scope")
        seen = set()
        current = span
        while current is not root:
            if current["span_id"] in seen:
                raise ValueError("Cycle in span ancestry")
            seen.add(current["span_id"])
            current = by_id.get(current["parent_span_id"])
            if current is None:
                raise ValueError("Missing parent span")
    return by_id


def verify_runtime(stderr, iters, spans_per_iter, output_bytes):
    records = {}
    for line in stderr.splitlines():
        if line.startswith(("WARMUP_VERIFIED ", "BENCH_RESULT ")):
            label, *fields = line.split()
            records[label] = dict(field.split("=", 1) for field in fields)
    for label, count in (("WARMUP_VERIFIED", max(1, iters // 10)), ("BENCH_RESULT", iters)):
        record = records[label]
        if (int(record["iters"]) != count or int(record["spans"]) != count * spans_per_iter
                or int(record["output_bytes"]) != output_bytes):
            raise ValueError(f"Invalid workload verification: {label}: {record}")
    return float(records["BENCH_RESULT"]["ns_per_iter"])


def main():
    global EVAL_DIR
    parser = argparse.ArgumentParser(description="Prepare pinned toml2json checkouts and evaluate dependencies")
    parser.add_argument("--source", default=APP_REPO, help="Git repository containing the pinned application commit")
    parser.add_argument("--offline", action="store_true", help="Use cached Cargo dependencies during setup")
    args = parser.parse_args()
    OUTPUT_DIR.mkdir(exist_ok=True)
    if (OUTPUT_DIR / "commands.jsonl").exists():
        raise RuntimeError(f"Results already exist in {OUTPUT_DIR}; preserve or move that directory before rerunning")
    with gzip.open(OUTPUT_DIR / "runner.py.gz", "wb") as stream:
        stream.write(Path(__file__).read_bytes())
    (ROOT_DIR / "target").mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="phase3-toml2json-", dir=ROOT_DIR / "target") as temporary:
        work = Path(temporary)
        clean = work / "untouched"
        EVAL_DIR = work / "evaluation"
        source = args.source if "://" in args.source else str(Path(args.source).resolve())
        run_cmd(["git", "clone", "--no-hardlinks", "--no-checkout", source, clean], cwd=ROOT_DIR)
        run_cmd(["git", "checkout", "--detach", APP_REVISION], cwd=clean)
        # The pinned application has no workspace table. Detach it from this parent workspace.
        with (clean / "Cargo.toml").open("a") as stream:
            stream.write("\n[workspace]\n")
        shutil.copytree(clean, EVAL_DIR, ignore=shutil.ignore_patterns(".git"))
        run_cmd(["git", "init", "--quiet"], cwd=EVAL_DIR)
        run_cmd(["git", "apply", EVIDENCE_DIR / "evaluation.patch"])
        shutil.copyfile(EVIDENCE_DIR / "untouched.Cargo.lock", clean / "Cargo.lock")
        shutil.copyfile(EVIDENCE_DIR / "evaluation.Cargo.lock", EVAL_DIR / "Cargo.lock")
        input_file = work / "test_input.toml"
        shutil.copyfile(EVIDENCE_DIR / "test_input.toml", input_file)
        _, rustc, _ = run_cmd(["rustc", "-vV"], cwd=ROOT_DIR)
        host = next(line.split(": ", 1)[1] for line in rustc.decode().splitlines() if line.startswith("host: "))
        for directory in (ROOT_DIR, clean, EVAL_DIR):
            run_cmd(["cargo", "fetch", "--locked", "--target", host,
                     *(["--offline"] if args.offline else [])], cwd=directory)
        run_cmd(["cargo", "build", "--locked", "--offline", "--release", "-p", "cargo-instrument", "--bin", "cargo-instrument"], cwd=ROOT_DIR)
        instrument = ROOT_DIR / "target/release/cargo-instrument"
        _, revision, _ = run_cmd(["git", "rev-parse", "HEAD"], cwd=ROOT_DIR)
        _, cargo, _ = run_cmd(["cargo", "-V"], cwd=ROOT_DIR)
        locks = {directory: sha256(directory / "Cargo.lock") for directory in (ROOT_DIR, clean, EVAL_DIR)}
        inputs = {name: sha256(EVAL_DIR / name) for name in ("src/main.rs", "Cargo.toml", "Cargo.lock")}
        save_json(OUTPUT_DIR / "manifest.json", {
            "captured_at": datetime.now(timezone.utc).isoformat(), "application_revision": APP_REVISION,
            "instrumentation_revision": revision.decode().strip(), "rustc": rustc.decode(), "cargo": cargo.decode(),
            "host": os.uname()._asdict() if hasattr(os.uname(), "_asdict") else list(os.uname()),
            "cpuinfo": Path("/proc/cpuinfo").read_text() if Path("/proc/cpuinfo").exists() else None,
            "memory": Path("/proc/meminfo").read_text() if Path("/proc/meminfo").exists() else None,
            "loadavg": os.getloadavg(), "lockfiles": {str(path): digest for path, digest in locks.items()},
            "inputs": inputs, "input_sha256": sha256(input_file),
            "patch_sha256": sha256(EVIDENCE_DIR / "evaluation.patch"),
            "runner_sha256": sha256(Path(__file__)), "instrument_binary_sha256": sha256(instrument),
            "telemetry": "AlwaysOn sampler, synchronous atomic counting processor; no network exporter",
            "build_flags": ["--release", "--locked", "--offline"],
        })
        targets = {variant: work / f"target-{variant}" for variant in ("untouched", "a", "b")}
        binaries = {variant: target / "release/toml2json" for variant, target in targets.items()}

        def build(variant):
            prefix = [instrument, "--with-dependencies", "--"] if variant == "b" else ["cargo"]
            return run_cmd([*prefix, "build", "--release", "--locked", "--offline", "--target-dir", targets[variant]],
                           cwd=clean if variant == "untouched" else EVAL_DIR)

        def verify_outputs_and_traces():
            outputs = {variant: run_cmd([binary, input_file])[1] for variant, binary in binaries.items()}
            if len(set(outputs.values())) != 1:
                raise ValueError("Application JSON output differs between variants")
            (work / "expected.json").write_bytes(outputs["untouched"])
            for variant, count in (("a", 0), ("b", 27)):
                _, _, stderr = run_cmd([binaries[variant], "--dump-spans", input_file])
                spans = [json.loads(line.removeprefix("SPAN_JSON ")) for line in stderr.splitlines() if line.startswith("SPAN_JSON ")]
                by_id = verify_trace(spans, count)
                if variant == "b":
                    # These direct calls are in clap_lex 1.0.0's RawArgs::{new,cursor,next}.
                    for child, parent in (("<RawArgs as From<I>>::from", "RawArgs::new"),
                                          ("ArgCursor::new", "RawArgs::cursor"),
                                          ("ParsedArg<'s>::new", "RawArgs::next")):
                        matches = [span for span in spans if span["name"] == child]
                        if not matches or any(by_id[span["parent_span_id"]]["name"] != parent for span in matches):
                            raise ValueError(f"Incorrect expected call edge: {parent} -> {child}")
                save_json(OUTPUT_DIR / f"variant-{variant}-spans.json", spans)
            print("Verified identical JSON, exact 1/28 span counts, trace ancestry, and source-derived call edges", flush=True)
            return outputs["untouched"]

        for variant in targets:
            _, _, stderr = build(variant)
            if variant == "b":
                for crate in ("clap_lex", "anstream", "anstyle_query"):
                    if f"crate={crate}] selecting native R-4 emitter" not in stderr:
                        raise ValueError(f"Missing native route proof for {crate}")
                if "selecting Tier-2" in stderr or "crate=toml2json] transformed" in stderr:
                    raise ValueError("Unexpected fallback or first-party instrumentation")
        expected = verify_outputs_and_traces()
        results = {"metadata": {"application": "toml2json", "revision": APP_REVISION, "benchmark_iterations": 5000,
                                 "spans_per_iter_variant_b": 18, "output_sha256": hashlib.sha256(expected).hexdigest()},
                   "build_diagnostics": []}
        for phase in ("clean_build_seconds", "repeat_build_seconds", "incremental_app_edit_seconds"):
            samples = {"a": [], "b": []}
            for index in range(5):
                for variant in (("a", "b") if index % 2 == 0 else ("b", "a")):
                    if phase == "clean_build_seconds":
                        shutil.rmtree(targets[variant])
                    elif phase == "incremental_app_edit_seconds":
                        os.utime(EVAL_DIR / "src/main.rs", None)
                    elapsed, _, stderr = build(variant)
                    samples[variant].append(elapsed)
                    results["build_diagnostics"].append({"phase": phase, "sample": index, "variant": variant,
                        "compiling_lines": [line.strip() for line in stderr.splitlines() if line.strip().startswith("Compiling ")],
                        "removed_lines": [line.strip() for line in stderr.splitlines() if line.strip().startswith("Removed ")]})
                    print(f"{phase} sample={index + 1} variant={variant} seconds={elapsed:.4f}", flush=True)
                save_json(OUTPUT_DIR / "partial-results.json", results | {"current_phase": phase, "samples": samples})
            a, b = compute_stats(samples["a"]), compute_stats(samples["b"])
            delta = b["median"] - a["median"]
            results[phase] = {"variant_a": a, "variant_b": b, "delta_abs": delta, "delta_pct": delta / a["median"] * 100}
        verify_outputs_and_traces()
        samples = {"a": [], "b": []}
        for index in range(10):
            for variant in (("a", "b") if index % 2 == 0 else ("b", "a")):
                _, _, stderr = run_cmd([binaries[variant], "--bench", "5000", input_file],
                                       extra_env={"EVAL_EXPECTED_JSON": str(work / "expected.json")})
                ns = verify_runtime(stderr, 5000, 18 if variant == "b" else 0, len(expected))
                samples[variant].append(ns)
                print(f"runtime sample={index + 1} variant={variant}: {stderr.strip()}", flush=True)
        a, b = compute_stats(samples["a"]), compute_stats(samples["b"])
        delta = b["median"] - a["median"]
        results["runtime_ns_per_iter"] = {"variant_a": a, "variant_b": b, "delta_abs": delta, "delta_pct": delta / a["median"] * 100}
        sizes = {variant: binary.stat().st_size for variant, binary in binaries.items()}
        results["binary_size_bytes"] = sizes
        if any(sha256(path / "Cargo.lock") != digest for path, digest in locks.items()):
            raise ValueError("A build changed a preserved lockfile")
        if any(sha256(EVAL_DIR / name) != digest for name, digest in inputs.items()):
            raise ValueError("A build changed an application source or manifest")
        results["verification"] = {"outputs_equal": True, "ancestry_verified": True,
                                   "warmup_and_runtime_verified": True, "source_and_lockfiles_unchanged": True}
        save_json(OUTPUT_DIR / "raw-results.json", results)
        (OUTPUT_DIR / "partial-results.json").unlink()
        print(f"Evaluation complete; results and full command logs: {OUTPUT_DIR}", flush=True)


if __name__ == "__main__":
    main()
