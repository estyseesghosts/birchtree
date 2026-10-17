#!/usr/bin/env bash
# Phase 5 smoke: the streaming API against the live binary. SSE is read with
# curl -N; WebSocket is read with a stdlib-only client (python3 -I), since the
# standard library has no WebSocket client. Covers: health, refusals and their
# text, every event the phase emits (update, status.update, delete,
# notification), a block's retraction, the live block, the WebSocket
# subscribe/frame protocol and the WebSocket refusal header, and revocation
# closing an open stream.
# Ends with SMOKE_PHASE5_OK.
set -euo pipefail

cd "$(dirname "$0")/.."

PORT="${BIRCHTREE_SMOKE_PORT:-38931}"
BASE="http://127.0.0.1:${PORT}"
DATA_DIR="$(mktemp -d)"
BIN="${BIRCHTREE_BIN:-target/debug/birchtree}"
PID=""
STREAM_PIDS=()
WORK="$(mktemp -d)"

cleanup() {
  for p in "${STREAM_PIDS[@]:-}"; do
    [ -n "$p" ] && kill "$p" 2>/dev/null || true
  done
  if [ -n "$PID" ]; then
    kill -TERM "$PID" 2>/dev/null || true
    wait "$PID" 2>/dev/null || true
  fi
  rm -rf "$DATA_DIR" "$WORK"
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

# code METHOD PATH TOKEN [curl args...] -> HTTP status
code() {
  local method="$1" path="$2" token="${3:-}"
  local args=(-s -o /dev/null -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(-H "Authorization: Bearer $token")
  if [ "$#" -gt 3 ]; then args+=("${@:4}"); fi
  curl "${args[@]}"
}

# wait_for FILE PATTERN [SECONDS]: poll a stream capture for a line pattern.
wait_for() {
  local file="$1" pattern="$2" secs="${3:-5}"
  for _ in $(seq 1 $((secs * 10))); do
    grep -q -- "$pattern" "$file" 2>/dev/null && return 0
    sleep 0.1
  done
  echo "ASSERT FAILED: '$pattern' never reached $file" >&2
  cat "$file" >&2 || true
  exit 1
}

# open_stream TOKEN PATH FILE: curl -N into FILE in the background.
open_stream() {
  local token="$1" path="$2" file="$3"
  curl -sN -H "Authorization: Bearer $token" "$BASE$path" >"$file" 2>/dev/null &
  STREAM_PIDS+=("$!")
  echo "$!"
}

cargo build --bin birchtree >/dev/null

echo "==> start server"
"$BIN" --data-dir="$DATA_DIR" --listen="127.0.0.1:${PORT}" --public-url="$BASE" --registration-open &
PID=$!
for _ in $(seq 1 100); do
  curl -fsS "$BASE/health" >/dev/null 2>&1 && break
  sleep 0.1
done

echo "==> setup: app, client-credentials token, three accounts"
ALL_SCOPES="read write follow profile read:accounts write:accounts read:follows write:follows read:statuses write:statuses read:favourites write:favourites read:notifications write:notifications read:lists write:lists read:blocks write:blocks"
APP_JSON="$(curl -fsS -X POST "$BASE/api/v1/apps" \
  --data-urlencode "client_name=Phase5 Smoke" \
  --data-urlencode "redirect_uris=https://client.example/cb" \
  --data-urlencode "scopes=$ALL_SCOPES")"
CLIENT_ID="$(printf '%s' "$APP_JSON" | json_get client_id)"
CLIENT_SECRET="$(printf '%s' "$APP_JSON" | json_get client_secret)"
CC_TOKEN="$(curl -fsS -X POST "$BASE/oauth/token" \
  --data-urlencode "grant_type=client_credentials" \
  --data-urlencode "client_id=$CLIENT_ID" \
  --data-urlencode "client_secret=$CLIENT_SECRET" \
  --data-urlencode "scope=$ALL_SCOPES" | json_get access_token)"

register() {
  curl -fsS -X POST "$BASE/api/v1/accounts" \
    -H "Authorization: Bearer $CC_TOKEN" \
    --data-urlencode "username=$1" \
    --data-urlencode "email=$1@example.com" \
    --data-urlencode "password=smokepassword" \
    --data-urlencode "agreement=true" | json_get access_token
}
ALICE="$(register alice5)"
BOB="$(register bob5)"
CAROL="$(register carol5)"
[ -n "$ALICE" ] && [ -n "$BOB" ] && [ -n "$CAROL" ]
ALICE_ID="$(curl -fsS "$BASE/api/v1/accounts/verify_credentials" -H "Authorization: Bearer $ALICE" | json_get id)"
BOB_ID="$(curl -fsS "$BASE/api/v1/accounts/verify_credentials" -H "Authorization: Bearer $BOB" | json_get id)"

echo "==> refusals and the health probe"
[ "$(curl -fsS "$BASE/api/v1/streaming/health")" = "OK" ]
[ "$(code GET /api/v1/streaming/public)" = "401" ]
curl -s "$BASE/api/v1/streaming/public" | grep -q '"error":"Missing access token"'
[ "$(code GET /api/v1/streaming/nope "$ALICE")" = "400" ]
curl -s "$BASE/api/v1/streaming/nope" | grep -q '"error":"Unknown channel requested"'
[ "$(code GET /api/v1/streaming/hashtag "$ALICE")" = "400" ]

echo "==> public stream: create, edit, delete"
PUB_FILE="$WORK/public.txt"
open_stream "$BOB" "/api/v1/streaming/public" "$PUB_FILE" >/dev/null
sleep 0.5
head -n 1 "$PUB_FILE" | grep -q '^:)$'
S1="$(curl -fsS -X POST "$BASE/api/v1/statuses" -H "Authorization: Bearer $BOB" \
  --data-urlencode "status=hello from bob" --data-urlencode "visibility=public" | json_get id)"
wait_for "$PUB_FILE" "^event: update$"
wait_for "$PUB_FILE" "\"id\":\"$S1\""
curl -fsS -X PUT "$BASE/api/v1/statuses/$S1" -H "Authorization: Bearer $BOB" \
  --data-urlencode "status=hello again" >/dev/null
wait_for "$PUB_FILE" "^event: status.update$"
curl -fsS -X DELETE "$BASE/api/v1/statuses/$S1" -H "Authorization: Bearer $BOB" >/dev/null
wait_for "$PUB_FILE" "^data: $S1\$"

echo "==> home stream: follow, deliver, block retracts, block stays silent"
HOME_FILE="$WORK/home.txt"
open_stream "$ALICE" "/api/v1/streaming/user" "$HOME_FILE" >/dev/null
sleep 0.5
curl -fsS -X POST "$BASE/api/v1/accounts/$BOB_ID/follow" -H "Authorization: Bearer $ALICE" >/dev/null
S2="$(curl -fsS -X POST "$BASE/api/v1/statuses" -H "Authorization: Bearer $BOB" \
  --data-urlencode "status=followed post" --data-urlencode "visibility=public" | json_get id)"
wait_for "$HOME_FILE" "\"id\":\"$S2\""
curl -fsS -X POST "$BASE/api/v1/accounts/$BOB_ID/block" -H "Authorization: Bearer $ALICE" >/dev/null
wait_for "$HOME_FILE" "^data: $S2\$"
S3="$(curl -fsS -X POST "$BASE/api/v1/statuses" -H "Authorization: Bearer $BOB" \
  --data-urlencode "status=after the block" --data-urlencode "visibility=public" | json_get id)"
sleep 1.5
if grep -q "$S3" "$HOME_FILE"; then
  echo "ASSERT FAILED: a status created after the block reached the blocker" >&2
  exit 1
fi

echo "==> notification stream: a favourite notifies the author's stream"
NOTE_FILE="$WORK/notes.txt"
open_stream "$BOB" "/api/v1/streaming/user/notification" "$NOTE_FILE" >/dev/null
sleep 0.5
S4="$(curl -fsS -X POST "$BASE/api/v1/statuses" -H "Authorization: Bearer $BOB" \
  --data-urlencode "status=fav me" --data-urlencode "visibility=public" | json_get id)"
curl -fsS -X POST "$BASE/api/v1/statuses/$S4/favourite" -H "Authorization: Bearer $CAROL" >/dev/null
wait_for "$NOTE_FILE" "^event: notification$" 8
wait_for "$NOTE_FILE" "\"type\":\"favourite\""

echo "==> WebSocket: upgrade refusal header, then the public frame"
python3 -I - "$PORT" "$CAROL" "$WORK/ws-refusal.txt" <<'PY'
import base64, os, socket, sys
port, token, out = int(sys.argv[1]), sys.argv[2], sys.argv[3]
key = base64.b64encode(os.urandom(16)).decode()
s = socket.create_connection(("127.0.0.1", port), timeout=10)
s.sendall((
    "GET /api/v1/streaming?stream=public HTTP/1.1\r\nHost: 127.0.0.1\r\n"
    "Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\n"
    f"Sec-WebSocket-Key: {key}\r\n\r\n"
).encode())
head = b""
while b"\r\n\r\n" not in head:
    chunk = s.recv(1024)
    if not chunk:
        break
    head += chunk
open(out, "w").write(head.decode("latin-1"))
s.close()
PY
grep -q "^HTTP/1.1 401" "$WORK/ws-refusal.txt"
grep -qi "^x-error-message: Missing access token" "$WORK/ws-refusal.txt"

python3 -I - "$PORT" "$BOB" "$CAROL" "$WORK/ws-frame.txt" <<'PY'
import base64, json, os, socket, struct, sys, threading, time, urllib.request
port, viewer, author, out = int(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4]
base = f"http://127.0.0.1:{port}"

def connect(path, token):
    key = base64.b64encode(os.urandom(16)).decode()
    s = socket.create_connection(("127.0.0.1", port), timeout=10)
    s.sendall((
        f"GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\n"
        f"Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\n"
        f"Authorization: Bearer {token}\r\n\r\n"
    ).encode())
    buf = b""
    while b"\r\n\r\n" not in buf:
        buf += s.recv(1024)
    head, rest = buf.split(b"\r\n\r\n", 1)
    assert head.startswith(b"HTTP/1.1 101"), head[:40]
    return s, rest

def read_frame(s, rest):
    def take(n):
        nonlocal rest
        while len(rest) < n:
            rest += s.recv(4096)
        data, rest = rest[:n], rest[n:]
        return data
    b1, b2 = take(2)
    length = b2 & 0x7F
    if length == 126:
        length = struct.unpack(">H", take(2))[0]
    elif length == 127:
        length = struct.unpack(">Q", take(8))[0]
    payload = take(length)
    return b1 & 0x0F, payload, rest

s, rest = connect("/api/v1/streaming?stream=public", viewer)
def post_status():
    time.sleep(0.4)
    req = urllib.request.Request(
        f"{base}/api/v1/statuses", method="POST",
        data=b"status=ws+frame&visibility=public",
        headers={"Authorization": f"Bearer {author}",
                 "Content-Type": "application/x-www-form-urlencoded"})
    urllib.request.urlopen(req, timeout=10).read()
threading.Thread(target=post_status, daemon=True).start()

deadline = time.time() + 8
frame = None
while time.time() < deadline:
    opcode, payload, rest = read_frame(s, rest)
    if opcode == 1:
        doc = json.loads(payload.decode())
        if doc.get("event") == "update":
            frame = doc
            break
    elif opcode == 9:
        continue
    elif opcode == 8:
        break
assert frame is not None, "no update frame"
assert frame["stream"] == ["public"], frame["stream"]
status = json.loads(frame["payload"])
assert status["content"].find("ws frame") >= 0, status["content"]
with open(out, "w") as fh:
    fh.write(json.dumps({"stream": frame["stream"], "event": frame["event"]}))
s.close()
PY
grep -q '"event": "update"' "$WORK/ws-frame.txt"

echo "==> revocation closes an open stream"
REVOKE_BODY="client_id=$CLIENT_ID&client_secret=$CLIENT_SECRET&token=$ALICE"
[ "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/oauth/revoke" --data "$REVOKE_BODY")" = "200" ]
for _ in $(seq 1 50); do
  kill -0 "${STREAM_PIDS[1]}" 2>/dev/null || break
  sleep 0.1
done
if kill -0 "${STREAM_PIDS[1]}" 2>/dev/null; then
  echo "ASSERT FAILED: the home stream is still open after revoke" >&2
  exit 1
fi

echo "SMOKE_PHASE5_OK"
