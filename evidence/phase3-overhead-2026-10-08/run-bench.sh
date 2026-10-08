#!/usr/bin/env bash
# Phase 3 overhead benchmark three-session execution — 2026-10-08
set -o pipefail
cd /home/brandybuck/Code/rust-compile-time-instrumentation-

SESSION="${1:-1}"
LOG="evidence/phase3-overhead-2026-10-08/session${SESSION}.log"

mkdir -p evidence/phase3-overhead-2026-10-08
{
  echo "SESSION=$SESSION"
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

echo "session $SESSION finished with exit $status"
exit $status
