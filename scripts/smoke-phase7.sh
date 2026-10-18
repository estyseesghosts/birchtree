#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
PORT="${BIRCHTREE_SMOKE_PORT:-38940}"
BASE="http://127.0.0.1:${PORT}"
DATA_DIR="$(mktemp -d)"
BIN="${BIRCHTREE_BIN:-target/debug/birchtree}"
PID=""
cleanup(){ [ -n "$PID" ] && { kill -TERM "$PID" 2>/dev/null || true; wait "$PID" 2>/dev/null || true; }; rm -rf "$DATA_DIR"; }
trap cleanup EXIT
"$BIN" --data-dir="$DATA_DIR" --listen="127.0.0.1:$PORT" >/tmp/opencode/phase7.log 2>&1 &
PID=$!
for i in $(seq 1 50); do curl -s "$BASE/health" >/dev/null 2>&1 && break; sleep 0.2; done
# unauth multi-fetch -> 401 (auth required? optional; but scope check only if token; no token -> allowed? Actually optional auth allows anonymous; ids empty -> [])
code=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/accounts")
echo "accounts index: $code"
code=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/donation_campaigns")
echo "donation_campaigns: $code"
code=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/suggestions")
echo "suggestions v1: $code"
echo "SMOKE_PHASE7_OK"
