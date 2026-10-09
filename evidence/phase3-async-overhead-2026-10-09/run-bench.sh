#!/usr/bin/env bash
# Phase 3 async overhead benchmark execution runner (2026-10-09)
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT_DIR"

SESSION="${1:-1}"
OUT_DIR="${OUT_DIR:-evidence/phase3-async-overhead-2026-10-09}"
LOG="$OUT_DIR/session${SESSION}.log"

if [ -e "$LOG" ]; then
  echo "Error: refusing to overwrite existing log $LOG" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"
{
  echo "SESSION=$SESSION"
  echo "START_UTC=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "LOADAVG_START=$(cat /proc/loadavg)"
  echo "COMMAND=CARGO_NET_OFFLINE=true cargo bench -p cargo-instrument --bench bench_async_overhead"
} > "$LOG"

set +e
CARGO_NET_OFFLINE=true cargo bench -p cargo-instrument --bench bench_async_overhead >> "$LOG" 2>&1
status=$?
set -e

{
  echo "BENCH_EXIT=$status"
  echo "END_UTC=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "LOADAVG_END=$(cat /proc/loadavg)"
} >> "$LOG"

echo "session $SESSION finished with exit $status (log: $LOG)"
exit $status
