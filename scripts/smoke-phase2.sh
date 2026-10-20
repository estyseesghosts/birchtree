#!/usr/bin/env bash
# Phase 2 smoke: run the real binary and drive the OAuth + registration surface
# with curl. Covers app registration, client-credentials, account registration,
# verify_credentials, the browser authorization-code + PKCE S256 flow, and the
# open/closed registration policy. Ends with SMOKE_PHASE2_OK.
set -euo pipefail

cd "$(dirname "$0")/.."

PORT="${BIRCHTREE_SMOKE_PORT:-38918}"
BASE="http://127.0.0.1:${PORT}"
DATA_DIR="$(mktemp -d)"
COOKIE_JAR="$(mktemp)"
BIN="${BIRCHTREE_BIN:-target/debug/birchtree}"
PID=""

cleanup() {
  if [ -n "$PID" ]; then
    kill -TERM "$PID" 2>/dev/null || true
    wait "$PID" 2>/dev/null || true
  fi
  rm -rf "$DATA_DIR" "$COOKIE_JAR"
}
trap cleanup EXIT

# Extract a top-level string field from a JSON document on stdin.
json_get() {
  python3 -c 'import sys,json; print(json.load(sys.stdin).get(sys.argv[1],""))' "$1"
}

# Extract a single query parameter from a URL.
url_param() {
  python3 -c 'import sys,urllib.parse as u; print(u.parse_qs(u.urlparse(sys.argv[1]).query).get(sys.argv[2],[""])[0])' "$1" "$2"
}

cargo build --bin birchtree >/dev/null

start_server() {
  "$BIN" --data-dir="$DATA_DIR" --listen="127.0.0.1:${PORT}" --public-url="$BASE" --no-transcode "$@" &
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

echo "==> start server (registration open)"
start_server --registration-open

echo "==> instance endpoints"
[ "$(curl -fsS "$BASE/api/v1/instance" | json_get title)" = "Birchtree" ]
[ "$(curl -fsS "$BASE/api/v2/instance" | json_get domain)" = "127.0.0.1" ]

echo "==> register OAuth application"
APP_JSON="$(curl -fsS -X POST "$BASE/api/v1/apps" \
  --data-urlencode "client_name=Smoke Client" \
  --data-urlencode "redirect_uris=https://client.example/cb" \
  --data-urlencode "scopes=read write profile read:accounts write:accounts")"
CLIENT_ID="$(printf '%s' "$APP_JSON" | json_get client_id)"
CLIENT_SECRET="$(printf '%s' "$APP_JSON" | json_get client_secret)"
[ -n "$CLIENT_ID" ]
[ -n "$CLIENT_SECRET" ]

echo "==> client-credentials token"
CC_JSON="$(curl -fsS -X POST "$BASE/oauth/token" \
  --data-urlencode "grant_type=client_credentials" \
  --data-urlencode "client_id=$CLIENT_ID" \
  --data-urlencode "client_secret=$CLIENT_SECRET" \
  --data-urlencode "scope=write:accounts read")"
CC_TOKEN="$(printf '%s' "$CC_JSON" | json_get access_token)"
[ -n "$CC_TOKEN" ]

echo "==> app-only token is rejected on a user endpoint"
code="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/accounts/verify_credentials" \
  -H "Authorization: Bearer $CC_TOKEN")"
[ "$code" = "422" ]

echo "==> register account (needs the client-credentials token)"
REG_JSON="$(curl -fsS -X POST "$BASE/api/v1/accounts" \
  -H "Authorization: Bearer $CC_TOKEN" \
  --data-urlencode "username=smokey" \
  --data-urlencode "email=smokey@example.com" \
  --data-urlencode "password=smokepassword" \
  --data-urlencode "agreement=true")"
USER_TOKEN="$(printf '%s' "$REG_JSON" | json_get access_token)"
[ -n "$USER_TOKEN" ]

echo "==> verify_credentials"
[ "$(curl -fsS "$BASE/api/v1/accounts/verify_credentials" \
  -H "Authorization: Bearer $USER_TOKEN" | json_get username)" = "smokey" ]

echo "==> preferences"
[ "$(curl -fsS "$BASE/api/v1/preferences" \
  -H "Authorization: Bearer $USER_TOKEN" | json_get 'posting:default:visibility')" = "public" ]

echo "==> authorization-code + PKCE S256 flow"
VERIFIER="dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
CHALLENGE="$(python3 -c 'import hashlib,base64,sys; print(base64.urlsafe_b64encode(hashlib.sha256(sys.argv[1].encode()).digest()).rstrip(b"=").decode())' "$VERIFIER")"

AUTH_PARAMS=(
  --data-urlencode "client_id=$CLIENT_ID"
  --data-urlencode "redirect_uri=https://client.example/cb"
  --data-urlencode "response_type=code"
  --data-urlencode "scope=read"
  --data-urlencode "state=st4te"
  --data-urlencode "code_challenge=$CHALLENGE"
  --data-urlencode "code_challenge_method=S256"
)

# Login establishes the browser session cookie.
curl -fsS -c "$COOKIE_JAR" -X POST "$BASE/oauth/authorize" \
  "${AUTH_PARAMS[@]}" \
  --data-urlencode "username=smokey" \
  --data-urlencode "password=smokepassword" >/dev/null

# Consent approves the request and redirects back with the one-time code.
LOCATION="$(curl -sS -i -b "$COOKIE_JAR" -X POST "$BASE/oauth/authorize" \
  "${AUTH_PARAMS[@]}" \
  --data-urlencode "decision=authorize" \
  | awk 'tolower($1)=="location:"{print $2}' | tr -d '\r')"
CODE="$(url_param "$LOCATION" code)"
[ -n "$CODE" ]

PKCE_JSON="$(curl -fsS -X POST "$BASE/oauth/token" \
  --data-urlencode "grant_type=authorization_code" \
  --data-urlencode "code=$CODE" \
  --data-urlencode "redirect_uri=https://client.example/cb" \
  --data-urlencode "code_verifier=$VERIFIER" \
  --data-urlencode "client_id=$CLIENT_ID" \
  --data-urlencode "client_secret=$CLIENT_SECRET")"
PKCE_TOKEN="$(printf '%s' "$PKCE_JSON" | json_get access_token)"
[ -n "$PKCE_TOKEN" ]
[ "$(curl -fsS "$BASE/api/v1/accounts/verify_credentials" \
  -H "Authorization: Bearer $PKCE_TOKEN" | json_get username)" = "smokey" ]

echo "==> authorization code is single-use"
code="$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/oauth/token" \
  --data-urlencode "grant_type=authorization_code" \
  --data-urlencode "code=$CODE" \
  --data-urlencode "redirect_uri=https://client.example/cb" \
  --data-urlencode "code_verifier=$VERIFIER" \
  --data-urlencode "client_id=$CLIENT_ID" \
  --data-urlencode "client_secret=$CLIENT_SECRET")"
[ "$code" = "400" ]

echo "==> revoke"
curl -fsS -X POST "$BASE/oauth/revoke" \
  --data-urlencode "token=$PKCE_TOKEN" \
  --data-urlencode "client_id=$CLIENT_ID" \
  --data-urlencode "client_secret=$CLIENT_SECRET" >/dev/null
code="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/preferences" \
  -H "Authorization: Bearer $PKCE_TOKEN")"
[ "$code" = "401" ]

echo "==> restart with registration closed"
stop_server
start_server

code="$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/api/v1/accounts" \
  -H "Authorization: Bearer $CC_TOKEN" \
  --data-urlencode "username=closed" \
  --data-urlencode "email=closed@example.com" \
  --data-urlencode "password=smokepassword" \
  --data-urlencode "agreement=true")"
[ "$code" = "403" ]

# Registration state is reflected by the v2 instance endpoint.
[ "$(curl -fsS "$BASE/api/v2/instance" | python3 -c 'import sys,json; print(json.load(sys.stdin)["registrations"]["enabled"])')" = "False" ]

echo "==> graceful shutdown"
stop_server

echo "SMOKE_PHASE2_OK"
