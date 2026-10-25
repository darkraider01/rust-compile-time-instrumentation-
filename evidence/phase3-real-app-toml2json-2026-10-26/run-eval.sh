#!/usr/bin/env bash
set -euo pipefail

EVIDENCE_DIR="$(cd "$(dirname "$0")" && pwd)"
if [[ -e "$EVIDENCE_DIR/validated/commands.jsonl" ]]; then
  echo "Results already exist in $EVIDENCE_DIR/validated; preserve or move that directory before rerunning" >&2
  exit 1
fi
mkdir -p "$EVIDENCE_DIR/validated"
python3 -u "$EVIDENCE_DIR/run-eval.py" "$@" 2>&1 | tee "$EVIDENCE_DIR/validated/session.log"
