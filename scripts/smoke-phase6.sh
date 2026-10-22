#!/usr/bin/env bash
# Phase 6 smoke: federation against the live binary, then the in-process suite.
# Covers discovery (WebFinger, actor document, NodeInfo), the inbox's refusals
# (an unsupported or malformed activity is 422, an unknown personal inbox is 404),
# and the crate-backed in-process tests (tests/phase6_federation.rs).
#
# Two-instance interop with vanilla Mastodon is not part of this smoke: it needs
# Docker and the lab network. See scripts/lab/phase6-interop.sh.
# Ends with SMOKE_PHASE6_OK.
set -euo pipefail

cd "$(dirname "$0")/.."

PORT="${BIRCHTREE_SMOKE_PORT:-38932}"
BASE="http://127.0.0.1:${PORT}"
HOST="127.0.0.1:${PORT}"
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

json_get() {
  python3 -I -c '
import sys, json
doc = json.load(sys.stdin)
cur = doc
for part in sys.argv[1].split("."):
    if part == "":
        continue
    cur = cur[int(part)] if isinstance(cur, list) else cur.get(part)
print("" if cur is None else cur)
' "$1"
}

# code METHOD PATH [curl args...] -> HTTP status
code() {
  local method="$1" path="$2"
  shift 2
  curl -s -o /dev/null -w '%{http_code}' -X "$method" "$BASE$path" "$@"
}

assert_code() {
  local want="$1" got="$2" what="$3"
  if [ "$got" != "$want" ]; then
    echo "ASSERT FAILED: $what: want $want, got $got" >&2
    exit 1
  fi
  echo "    ok: $what -> $got"
}

assert_eq() {
  if [ "$1" != "$2" ]; then
    echo "ASSERT FAILED: $3: want '$2', got '$1'" >&2
    exit 1
  fi
  echo "    ok: $3"
}

cargo build --bin birchtree >/dev/null

echo "==> start server"
"$BIN" --data-dir="$DATA_DIR" --listen="127.0.0.1:${PORT}" --public-url="$BASE" --registration-open --no-transcode --smtp-url=smtp://127.0.0.1:25 --mail-from=noreply@example.social &
PID=$!
for _ in $(seq 1 100); do
  curl -fsS "$BASE/health" >/dev/null 2>&1 && break
  sleep 0.1
done

echo "==> setup: client-credentials token and one local account"
ALL_SCOPES="read write follow profile read:accounts write:accounts read:follows write:follows"
APP_JSON="$(curl -fsS -X POST "$BASE/api/v1/apps" \
  --data-urlencode "client_name=Phase6 Smoke" \
  --data-urlencode "redirect_uris=https://client.example/cb" \
  --data-urlencode "scopes=$ALL_SCOPES")"
CLIENT_ID="$(printf '%s' "$APP_JSON" | json_get client_id)"
CLIENT_SECRET="$(printf '%s' "$APP_JSON" | json_get client_secret)"
CC_TOKEN="$(curl -fsS -X POST "$BASE/oauth/token" \
  --data-urlencode "grant_type=client_credentials" \
  --data-urlencode "client_id=$CLIENT_ID" \
  --data-urlencode "client_secret=$CLIENT_SECRET" \
  --data-urlencode "scope=$ALL_SCOPES" | json_get access_token)"
curl -fsS -X POST "$BASE/api/v1/accounts" \
  -H "Authorization: Bearer $CC_TOKEN" \
  --data-urlencode "username=alice6" \
  --data-urlencode "email=alice6@example.com" \
  --data-urlencode "password=smokepassword" \
  --data-urlencode "agreement=true" >/dev/null

echo "==> discovery"
WF="$(curl -fsS "$BASE/.well-known/webfinger?resource=acct:alice6@$HOST")"
assert_eq "$(printf '%s' "$WF" | json_get subject)" "acct:alice6@$HOST" "webfinger subject names the local account"
assert_eq "$(printf '%s' "$WF" | python3 -I -c 'import json,sys; print([l["href"] for l in json.load(sys.stdin)["links"] if l["rel"]=="self"][0])')" "$BASE/users/alice6" "webfinger self link is the actor"
assert_code 404 "$(code GET "/.well-known/webfinger?resource=acct:nobody@$HOST")" "webfinger refuses an unknown name"

ACTOR="$(curl -fsS -H 'Accept: application/activity+json' "$BASE/users/alice6")"
assert_eq "$(printf '%s' "$ACTOR" | json_get type)" "Person" "actor document is a Person"
assert_eq "$(printf '%s' "$ACTOR" | json_get publicKey.id)" "$BASE/users/alice6#main-key" "actor publishes the main key"
assert_code 404 "$(code GET /users/nobody)" "actor refuses an unknown name"

NODEINFO="$(curl -fsS "$BASE/nodeinfo/2.0")"
assert_eq "$(printf '%s' "$NODEINFO" | json_get version)" "2.0" "nodeinfo 2.0"

# The crate checks the body digest before it parses, so a POST without a Digest
# header is refused as unauthenticated. Mastodon always sends one.
digest_of() { printf '%s' "$1" | python3 -I -c 'import sys,hashlib,base64; print(base64.b64encode(hashlib.sha256(sys.stdin.buffer.read()).digest()).decode())'; }
post_activity() { # post_activity PATH BODY -> status, with a correct Digest
  code POST "$1" -H 'Content-Type: application/activity+json' \
    -H "Digest: SHA-256=$(digest_of "$2")" --data "$2"
}
LIKE='{"type":"Like","id":"https://x.example/1","actor":"https://x.example/u","object":"https://y.example/s"}'

echo "==> inbox refusals (no network, no state change)"
assert_code 401 "$(code POST /inbox -H 'Content-Type: application/activity+json' --data "$LIKE")" "an activity without a Digest is refused"
assert_code 422 "$(post_activity /inbox "$LIKE")" "unsupported activity type is refused (422)"
assert_code 422 "$(post_activity /inbox 'not json')" "malformed body is refused (422)"
assert_code 404 "$(post_activity /users/nobody/inbox '{}')" "personal inbox of an unknown account is refused (404)"

echo "==> in-process suite"
cargo test --test phase6_federation --quiet

echo "SMOKE_PHASE6_OK"
