#!/usr/bin/env bash
# Phase 4 smoke: the real-client media/post/timeline/notification workflow
# against the live binary with curl — media upload (v1/v2 + processing
# lifecycle), posting with attachments, home/public/tag/list timelines with
# Link cursors, the notification read surface + markers, lists, scheduled
# statuses, search and the custom-emoji index.
# Ends with SMOKE_PHASE4_OK.
set -euo pipefail

cd "$(dirname "$0")/.."

PORT="${BIRCHTREE_SMOKE_PORT:-38929}"
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

json_get() {
  python3 -c '
import sys, json
doc = json.load(sys.stdin)
cur = doc
for part in sys.argv[1].split("."):
    if part == "":
        continue
    if isinstance(cur, list):
        cur = cur[int(part)]
    elif isinstance(cur, dict):
        cur = cur.get(part)
    else:
        cur = None
        break
if cur is None:
    print("")
elif cur is True:
    print("true")
elif cur is False:
    print("false")
else:
    print(cur)
' "$1"
}

jassert() {
  local doc="$1" path="$2" want="$3" what="$4"
  local got
  got="$(DOC="$doc" PATH_="$path" python3 - <<'PY'
import json, os
doc = json.loads(os.environ["DOC"])
cur = doc
for part in os.environ["PATH_"].split("."):
    if part == "":
        continue
    if isinstance(cur, list):
        cur = cur[int(part)]
    elif isinstance(cur, dict):
        cur = cur.get(part)
    else:
        cur = None
        break
if cur is None:
    print("null")
elif cur is True:
    print("true")
elif cur is False:
    print("false")
else:
    print(cur)
PY
)"
  if [ "$got" != "$want" ]; then
    echo "ASSERT FAILED: $what: path $path = '$got', want '$want'" >&2
    echo "doc: $doc" >&2
    exit 1
  fi
}

# assert_code METHOD PATH TOKEN WANT [curl args...]
assert_code() {
  local method="$1" path="$2" token="$3" want="$4"; shift 4
  local args=(-s -o /dev/null -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(-H "Authorization: Bearer $token")
  args+=("$@")
  local got
  got="$(curl "${args[@]}")"
  if [ "$got" != "$want" ]; then
    echo "ASSERT FAILED: $method $path -> $got, want $want" >&2
    exit 1
  fi
}

# status_code METHOD PATH TOKEN [curl args...]
code() {
  local method="$1" path="$2" token="${3:-}"; shift 3 || true
  local args=(-s -o /dev/null -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(-H "Authorization: Bearer $token")
  args+=("$@")
  curl "${args[@]}"
}

cargo build --bin birchtree >/dev/null

echo "==> start server"
"$BIN" --data-dir="$DATA_DIR" --listen="127.0.0.1:${PORT}" --public-url="$BASE" --registration-open --no-transcode --smtp-url=smtp://127.0.0.1:25 --mail-from=noreply@example.social &
PID=$!
for _ in $(seq 1 100); do
  curl -fsS "$BASE/health" >/dev/null 2>&1 && break
  sleep 0.1
done

echo "==> setup: app + two accounts"
ALL_SCOPES="read write follow profile read:accounts write:accounts read:follows write:follows read:statuses write:statuses read:favourites write:favourites read:notifications write:notifications read:lists write:lists read:search read:filters write:filters"
APP_JSON="$(curl -fsS -X POST "$BASE/api/v1/apps" \
  --data-urlencode "client_name=Phase4 Smoke" \
  --data-urlencode "redirect_uris=https://client.example/cb" \
  --data-urlencode "scopes=$ALL_SCOPES")"
CLIENT_ID="$(printf '%s' "$APP_JSON" | json_get client_id)"
CC_JSON="$(curl -fsS -X POST "$BASE/oauth/token" \
  --data-urlencode "grant_type=client_credentials" \
  --data-urlencode "client_id=$CLIENT_ID" \
  --data-urlencode "client_secret=$(printf '%s' "$APP_JSON" | json_get client_secret)" \
  --data-urlencode "scope=$ALL_SCOPES")"
CC_TOKEN="$(printf '%s' "$CC_JSON" | json_get access_token)"

register() {
  curl -fsS -X POST "$BASE/api/v1/accounts" \
    -H "Authorization: Bearer $CC_TOKEN" \
    --data-urlencode "username=$1" \
    --data-urlencode "email=$1@example.com" \
    --data-urlencode "password=smokepassword" \
    --data-urlencode "agreement=true" | json_get access_token
}
ALICE="$(register alice4)"
BOB="$(register bob4)"
[ -n "$ALICE" ] && [ -n "$BOB" ]
ALICE_ID="$(curl -fsS "$BASE/api/v1/accounts/verify_credentials" -H "Authorization: Bearer $ALICE" | json_get id)"
BOB_ID="$(curl -fsS "$BASE/api/v1/accounts/verify_credentials" -H "Authorization: Bearer $BOB" | json_get id)"

echo "==> media: v1 upload (multipart) -> 200 with processed urls"
python3 - <<PY
import struct, zlib, pathlib
w = h = 32
raw = b"".join(b"\x00" + bytes([i % 256, 80, 128]) * w for i in range(h))
def chunk(tag, data):
    return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data))
png = (b"\x89PNG\r\n\x1a\n"
       + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
       + chunk(b"IDAT", zlib.compress(raw))
       + chunk(b"IEND", b""))
pathlib.Path("$DATA_DIR/smoke.png").write_bytes(png)
# Minimal ISO-BMFF video-ish file (ftyp only): v2 delays it -> 202.
pathlib.Path("$DATA_DIR/smoke.mp4").write_bytes(
    b"\x00\x00\x00\x18ftypisom\x00\x00\x00\x00isomiso2" + b"\x00" * 64)
# A text file: content sniffing must reject it.
pathlib.Path("$DATA_DIR/smoke.txt").write_bytes(b"this is not media at all")
PY

MEDIA_JSON="$(curl -fsS -X POST "$BASE/api/v1/media" \
  -H "Authorization: Bearer $ALICE" \
  -F "file=@$DATA_DIR/smoke.png;type=image/png" \
  --form-string "description=a smoke png" \
  --form-string "focus=0.25,0.75")"
[ -n "$(printf '%s' "$MEDIA_JSON" | json_get id)" ] || { echo "media id missing" >&2; exit 1; }
MEDIA_ID="$(printf '%s' "$MEDIA_JSON" | json_get id)"
MEDIA_URL="$(printf '%s' "$MEDIA_JSON" | json_get url)"
MEDIA_PREVIEW="$(printf '%s' "$MEDIA_JSON" | json_get preview_url)"
[ -n "$MEDIA_ID" ]
case "$MEDIA_URL" in
  "$BASE/system/media_attachments/files/"*) ;;
  *) echo "unexpected media url: $MEDIA_URL" >&2; exit 1 ;;
esac
[ -n "$MEDIA_PREVIEW" ]
jassert "$MEDIA_JSON" "description" "a smoke png" "media description"
[ -n "$(printf '%s' "$MEDIA_JSON" | json_get blurhash)" ] || { echo "blurhash missing" >&2; exit 1; }
BLURHASH="$(printf '%s' "$MEDIA_JSON" | json_get blurhash)"
[ -n "$BLURHASH" ]
jassert "$MEDIA_JSON" "meta.focus.x" "0.25" "focus x"
jassert "$MEDIA_JSON" "meta.original.size" "32x32" "original size"
# Every documented media key is present.
for key in id type url preview_url remote_url preview_remote_url text_url meta description blurhash; do
  printf '%s' "$MEDIA_JSON" | python3 -c "import sys,json; d=json.load(sys.stdin); assert '$key' in d, 'missing $key'"
done

echo "==> media: serve the stored bytes"
FETCHED_CODE="$(code GET "${MEDIA_URL#"$BASE"}" "")"
[ "$FETCHED_CODE" = "200" ]
curl -fsS "$BASE${MEDIA_URL#"$BASE"}" -o "$DATA_DIR/fetched.png"
cmp -s "$DATA_DIR/smoke.png" "$DATA_DIR/fetched.png" || {
  echo "served bytes differ from the upload" >&2; exit 1
}

echo "==> media: update description/focus"
UPDATED="$(curl -fsS -X PUT "$BASE/api/v1/media/$MEDIA_ID" \
  -H "Authorization: Bearer $ALICE" \
  --data-urlencode "description=updated description" \
  --data-urlencode "focus=-0.5,0.5")"
jassert "$UPDATED" "description" "updated description" "media update description"
jassert "$UPDATED" "meta.focus.x" "-0.5" "updated focus x"

echo "==> media: v2 upload of a video-ish file -> 202 while processing"
assert_code POST "/api/v2/media" "$ALICE" 202 \
  -F "file=@$DATA_DIR/smoke.mp4;type=video/mp4"
V2_JSON="$(curl -fsS -X POST "$BASE/api/v2/media" \
  -H "Authorization: Bearer $ALICE" \
  -F "file=@$DATA_DIR/smoke.mp4;type=video/mp4")"
V2_ID="$(printf '%s' "$V2_JSON" | json_get id)"
jassert "$V2_JSON" "url" "null" "delayed upload url is null"
# The worker polls every 250ms and may finish before this read: a 206 must
# carry a null url, a 200 means it already finished, anything else fails.
V2_GET="$(curl -s -w '\n%{http_code}' -H "Authorization: Bearer $ALICE" "$BASE/api/v1/media/$V2_ID")"
V2_CODE="${V2_GET##*$'\n'}"
V2_BODY="${V2_GET%$'\n'*}"
case "$V2_CODE" in
  206) jassert "$V2_BODY" "url" "null" "processing media has a null url" ;;
  200) ;;
  *) echo "ASSERT FAILED: GET /api/v1/media/$V2_ID while processing -> $V2_CODE" >&2; exit 1 ;;
esac
# The ProcessMedia job finishes it; poll until 200.
for _ in $(seq 1 50); do
  [ "$(code GET "/api/v1/media/$V2_ID" "$ALICE")" = "200" ] && break
  sleep 0.1
done
[ "$(code GET "/api/v1/media/$V2_ID" "$ALICE")" = "200" ]

echo "==> media: rejected uploads"
assert_code POST "/api/v1/media" "$ALICE" 422 \
  -F "file=@$DATA_DIR/smoke.txt;type=text/plain"
assert_code POST "/api/v1/media" "$ALICE" 422 \
  --form-string "file="
assert_code POST "/api/v1/media" "" 401 -F "file=@$DATA_DIR/smoke.png"

echo "==> post a status with the attachment"
STATUS_JSON="$(curl -fsS -X POST "$BASE/api/v1/statuses" \
  -H "Authorization: Bearer $ALICE" \
  --data-urlencode "status=media smoke #smoke4" \
  --data-urlencode "media_ids[]=$MEDIA_ID")"
STATUS_ID="$(printf '%s' "$STATUS_JSON" | json_get id)"
[ -n "$STATUS_ID" ]
ATTACHED_TYPE="$(printf '%s' "$STATUS_JSON" | json_get media_attachments.0.type)"
[ "$ATTACHED_TYPE" = "image" ]
ATTACHED_URL="$(printf '%s' "$STATUS_JSON" | json_get media_attachments.0.url)"
[ -n "$ATTACHED_URL" ] && [ "$ATTACHED_URL" != "null" ]
# Attached media cannot be deleted (422) nor re-addressed (404 on show).
assert_code DELETE "/api/v1/media/$MEDIA_ID" "$ALICE" 422
assert_code GET "/api/v1/media/$MEDIA_ID" "$ALICE" 404

echo "==> follow + timelines"
curl -fsS -X POST "$BASE/api/v1/accounts/$BOB_ID/follow" -H "Authorization: Bearer $ALICE" >/dev/null
BOB_STATUS="$(curl -fsS -X POST "$BASE/api/v1/statuses" \
  -H "Authorization: Bearer $BOB" \
  --data-urlencode "status=bob posting #smoke4")"
BOB_STATUS_ID="$(printf '%s' "$BOB_STATUS" | json_get id)"

# Home (Link cursors present, both statuses ordered newest first).
HOME_HEADERS="$(curl -sD - -o /tmp/phase4_home.json -H "Authorization: Bearer $ALICE" \
  "$BASE/api/v1/timelines/home?limit=20")"
printf '%s' "$HOME_HEADERS" | head -1 | grep -q "200" || { echo "home not 200" >&2; exit 1; }
printf '%s' "$HOME_HEADERS" | grep -qi '^link:' || { echo "home missing Link header" >&2; exit 1; }
HOME_COUNT="$(python3 -c 'import json,sys; print(len(json.load(open("/tmp/phase4_home.json"))))')"
[ "$HOME_COUNT" -ge 2 ]

# Public timeline anonymously (with cache headers).
PUB_HEADERS="$(curl -sD - -o /tmp/phase4_pub.json "$BASE/api/v1/timelines/public")"
printf '%s' "$PUB_HEADERS" | grep -qi 'cache-control: max-age=15, public' || {
  echo "public timeline missing cache headers" >&2; printf '%s' "$PUB_HEADERS" >&2; exit 1
}
PUB_COUNT="$(python3 -c 'import json,sys; print(len(json.load(open("/tmp/phase4_pub.json"))))')"
[ "$PUB_COUNT" -ge 2 ]

# Tag timeline (normalized, includes replies/reblogs later in the suite).
TAG_JSON="$(curl -fsS "$BASE/api/v1/timelines/tag/smoke4")"
TAG_COUNT="$(printf '%s' "$TAG_JSON" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))')"
[ "$TAG_COUNT" -ge 2 ]
EMPTY_TAG="$(curl -fsS "$BASE/api/v1/timelines/tag/definitelynotagaz")"
[ "$(printf '%s' "$EMPTY_TAG" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))')" = "0" ]

# List timeline: create a list, add bob, his post appears.
LIST_JSON="$(curl -fsS -X POST "$BASE/api/v1/lists" \
  -H "Authorization: Bearer $ALICE" --data-urlencode "title=Smoke list")"
LIST_ID="$(printf '%s' "$LIST_JSON" | json_get id)"
[ -n "$LIST_ID" ]
curl -fsS -X POST "$BASE/api/v1/lists/$LIST_ID/accounts" \
  -H "Authorization: Bearer $ALICE" --data-urlencode "account_ids[]=$BOB_ID" >/dev/null
LIST_JSON="$(curl -fsS -H "Authorization: Bearer $ALICE" "$BASE/api/v1/timelines/list/$LIST_ID")"
[ "$(printf '%s' "$LIST_JSON" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))')" -ge 1 ]
assert_code GET "/api/v1/timelines/list/999999999" "$ALICE" 404
assert_code GET "/api/v1/timelines/home" "" 401

echo "==> notifications + markers"
curl -fsS -X POST "$BASE/api/v1/statuses/$STATUS_ID/favourite" \
  -H "Authorization: Bearer $BOB" >/dev/null
# The live worker runs the fan-out job.
NOTIF_JSON="[]"
for _ in $(seq 1 50); do
  NOTIF_JSON="$(curl -fsS -H "Authorization: Bearer $ALICE" "$BASE/api/v1/notifications")"
  [ "$(printf '%s' "$NOTIF_JSON" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))')" -ge 1 ] && break
  sleep 0.1
done
NOTIF_COUNT="$(printf '%s' "$NOTIF_JSON" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))')"
[ "$NOTIF_COUNT" -ge 1 ]
jassert "$NOTIF_JSON" "0.type" "favourite" "notification type"
jassert "$NOTIF_JSON" "0.group_key" "ungrouped-$(printf '%s' "$NOTIF_JSON" | json_get 0.id)" "group_key fallback"
NOTIF_ID="$(printf '%s' "$NOTIF_JSON" | json_get 0.id)"
UNREAD="$(curl -fsS -H "Authorization: Bearer $ALICE" "$BASE/api/v1/notifications/unread_count")"
jassert "$UNREAD" "count" "1" "unread count"
# Mark the newest read -> unread drops to 0.
MARKER="$(curl -fsS -X POST "$BASE/api/v1/markers" \
  -H "Authorization: Bearer $ALICE" \
  --data-urlencode "notifications[last_read_id]=$NOTIF_ID")"
jassert "$MARKER" "notifications.version" "1" "marker version"
UNREAD="$(curl -fsS -H "Authorization: Bearer $ALICE" "$BASE/api/v1/notifications/unread_count")"
jassert "$UNREAD" "count" "0" "unread after marker"
# Dismiss + clear.
assert_code POST "/api/v1/notifications/$NOTIF_ID/dismiss" "$ALICE" 200
assert_code POST "/api/v1/notifications/clear" "$ALICE" 200

echo "==> scheduled status"
SCHEDULED_AT="$(python3 -c 'from datetime import datetime, timedelta, timezone; print((datetime.now(timezone.utc) + timedelta(hours=1)).strftime("%Y-%m-%dT%H:%M:%SZ"))')"
SCHED_JSON="$(curl -fsS -X POST "$BASE/api/v1/statuses" \
  -H "Authorization: Bearer $ALICE" \
  --data-urlencode "status=scheduled smoke" \
  --data-urlencode "scheduled_at=$SCHEDULED_AT")"
jassert "$SCHED_JSON" "params.text" "scheduled smoke" "scheduled params"
SCHED_ID="$(printf '%s' "$SCHED_JSON" | json_get id)"
[ -n "$SCHED_ID" ]
SCHED_LIST="$(curl -fsS -H "Authorization: Bearer $ALICE" "$BASE/api/v1/scheduled_statuses")"
[ "$(printf '%s' "$SCHED_LIST" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))')" = "1" ]
assert_code DELETE "/api/v1/scheduled_statuses/$SCHED_ID" "$ALICE" 200

echo "==> search"
SEARCH_JSON="$(curl -fsS "$BASE/api/v2/search?q=alice4&type=accounts")"
jassert "$SEARCH_JSON" "accounts.0.username" "alice4" "search finds the account"
for key in accounts statuses hashtags collections; do
  printf '%s' "$SEARCH_JSON" | python3 -c "import sys,json; d=json.load(sys.stdin); assert '$key' in d, 'missing $key'"
done
assert_code GET "/api/v2/search" "" 400

echo "==> custom emoji index (public, empty)"
EMOJI_JSON="$(curl -fsS "$BASE/api/v1/custom_emojis")"
[ "$(printf '%s' "$EMOJI_JSON" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))')" = "0" ]

echo "==> Vary header on API responses"
curl -sD - -o /dev/null "$BASE/api/v1/custom_emojis" | grep -qi '^vary:.*Authorization' || {
  echo "missing Vary: Authorization" >&2; exit 1
}

echo "SMOKE_PHASE4_OK"
