#!/usr/bin/env bash
# Phase 3 overhead benchmark pilot run — 2026-10-07
set -o pipefail
cd /home/brandybuck/Code/rust-compile-time-instrumentation-
LOG=/tmp/opencode/phase3-bench-2026-10-07/bench_overhead_raw.log
mkdir -p /tmp/opencode/phase3-bench-2026-10-07
{
  echo "START_UTC=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "LOADAVG_START=$(cat /proc/loadavg)"
  echo "COMMAND=CARGO_NET_OFFLINE=true cargo bench -p cargo-instrument --bench bench_overhead"
} > "$LOG"
CARGO_NET_OFFLINE=true cargo bench -p cargo-instrument --bench bench_overhead >> "$LOG" 2>&1
status=$?
{
  echo "BENCH_EXIT=$status"
  echo "END_UTC=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "LOADAVG_END=$(cat /proc/loadavg)"
} >> "$LOG"
echo "bench finished with exit $status"
