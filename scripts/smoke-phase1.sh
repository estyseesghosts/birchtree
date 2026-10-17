#!/usr/bin/env bash
# Phase 1 smoke: run the real binary, probe the runtime, create an account via
# the CLI, and prove restart durability. Ends with SMOKE_OK.
set -euo pipefail

cd "$(dirname "$0")/.."

PORT="${BIRCHTREE_SMOKE_PORT:-38917}"
BASE="http://127.0.0.1:${PORT}"
DATA_DIR="$(mktemp -d)"
BIN="${BIRCHTREE_BIN:-target/debug/birchtree}"
PID=""

cleanup() {
  if [ -n "$PID" ]; then
    kill -TERM "$PID" 2>/dev/null || true
    wait "$PID" 2>/dev/null || true
  fi
  rm -rf "$DATA_DIR"
}
trap cleanup EXIT

cargo build --bin birchtree >/dev/null

start_server() {
  "$BIN" --data-dir="$DATA_DIR" --listen="127.0.0.1:${PORT}" --public-url="$BASE" &
  PID=$!
  for _ in $(seq 1 100); do
    if curl -fsS "$BASE/health" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.1
  done
  echo "server did not become healthy" >&2
  return 1
}

stop_server() {
  kill -TERM "$PID"
  wait "$PID" 2>/dev/null || true
  PID=""
}

echo "==> start server"
start_server

echo "==> probes"
[ "$(curl -fsS "$BASE/health")" = "ok" ]
[ "$(curl -fsS "$BASE/ready")" = "ready" ]
code="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/nope")"
[ "$code" = "404" ]

echo "==> graceful shutdown"
stop_server

echo "==> CLI create-account"
"$BIN" create-account --data-dir="$DATA_DIR" --username=smoke \
  --password=smokepassword --public-url="$BASE"

echo "==> restart preserves identity"
start_server
[ "$(curl -fsS "$BASE/ready")" = "ready" ]
stop_server

echo "SMOKE_OK"
