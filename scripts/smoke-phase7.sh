#!/usr/bin/env bash
# Phase 7 loose-ends smoke: conversations (+mutes), push subscriptions,
# stray Phase 7 rows, search and the media lifecycle over HTTP.
set -euo pipefail
cd "$(dirname "$0")/.."
PORT="${BIRCHTREE_SMOKE_PORT:-38940}"
BASE="http://127.0.0.1:${PORT}"
DATA_DIR="$(mktemp -d)"
BIN="${BIRCHTREE_BIN:-target/debug/birchtree}"
PID=""
# Fail-closed secrets (prod-2 item 4): production refuses to start without a
# keyring, so smoke mints a throwaway one (0600) inside the data dir.
KEYRING="$DATA_DIR/keyring"
printf '%s\n' '1:0000000000000000000000000000000000000000000000000000000000000000' > "$KEYRING"
chmod 600 "$KEYRING"
cleanup(){ [ -n "$PID" ] && { kill -TERM "$PID" 2>/dev/null || true; wait "$PID" 2>/dev/null || true; }; rm -rf "$DATA_DIR"; }
trap cleanup EXIT
cargo build --bin birchtree >/dev/null
"$BIN" --data-dir="$DATA_DIR" --listen="127.0.0.1:$PORT" --public-url="$BASE" --registration-open --no-transcode --smtp-url=smtp://127.0.0.1:25 --mail-from=noreply@example.social --secret-keyring-file="$KEYRING" >/tmp/opencode/phase7.log 2>&1 &
PID=$!
for i in $(seq 1 100); do curl -fsS "$BASE/health" >/dev/null 2>&1 && break; sleep 0.2; done

jget() { python3 -c "import sys,json; print(json.load(sys.stdin)$1)"; }

# --- register an app + two users -------------------------------------------
APP=$(curl -fsS -X POST "$BASE/api/v1/apps" \
  --data-urlencode "client_name=smoke7" \
  --data-urlencode "redirect_uris=http://127.0.0.1/cb" \
  --data-urlencode "scopes=read write follow read:accounts write:accounts read:statuses write:statuses read:search push write:mutes write:conversations")
CID=$(echo "$APP" | jget "['client_id']")
CSEC=$(echo "$APP" | jget "['client_secret']")
mkuser() {
  local name="$1"
  local tok
  tok=$(curl -fsS -X POST "$BASE/oauth/token" \
    --data-urlencode "grant_type=client_credentials" \
    --data-urlencode "client_id=$CID" \
    --data-urlencode "client_secret=$CSEC" \
    --data-urlencode "scope=write:accounts" | jget "['access_token']")
  curl -fsS -X POST "$BASE/api/v1/accounts" \
    --data-urlencode "username=$name" \
    --data-urlencode "email=$name@example.com" \
    --data-urlencode "password=password123" \
    --data-urlencode "agreement=true" \
    -H "Authorization: Bearer $tok" | jget "['access_token']"
}
ALICE=$(mkuser alice7)
BOB=$(mkuser bob7)

# --- conversations -----------------------------------------------------------
DM=$(curl -fsS -X POST "$BASE/api/v1/statuses" \
  -H "Authorization: Bearer $ALICE" \
  --data-urlencode "status=@bob7 hello direct" \
  --data-urlencode "visibility=direct" | jget "['id']")
CONVS=$(curl -fsS "$BASE/api/v1/conversations" -H "Authorization: Bearer $BOB")
echo "$CONVS" | python3 -c "import sys,json; items=json.load(sys.stdin); assert len(items)==1, items; assert items[0]['id']=='$DM', items; assert items[0]['unread'] is True, items"
CID_CONV="$DM"
curl -fsS -X POST "$BASE/api/v1/conversations/$CID_CONV/read" -H "Authorization: Bearer $BOB" >/dev/null
curl -fsS "$BASE/api/v1/conversations" -H "Authorization: Bearer $BOB" | python3 -c "import sys,json; assert json.load(sys.stdin)[0]['unread'] is False"
curl -fsS -X POST "$BASE/api/v1/statuses/$DM/mute" -H "Authorization: Bearer $BOB" | python3 -c "import sys,json; assert json.load(sys.stdin)['muted'] is True"
curl -fsS -X POST "$BASE/api/v1/statuses/$DM/unmute" -H "Authorization: Bearer $BOB" | python3 -c "import sys,json; assert json.load(sys.stdin)['muted'] is False"
curl -fsS -X DELETE "$BASE/api/v1/conversations/$CID_CONV" -H "Authorization: Bearer $BOB" >/dev/null
code=$(curl -s -o /dev/null -w '%{http_code}' -X DELETE "$BASE/api/v1/conversations/$CID_CONV" -H "Authorization: Bearer $BOB")
[ "$code" = "404" ] || { echo "second delete: $code"; exit 1; }
echo "conversations: OK"

# --- push subscriptions -------------------------------------------------------
P256="BFJ01YCiilwmVb6mR1XYS0clhLpMu2xhiDaA5nqZKjUMp_AknV2VL0wWCZWyyGnlY_Mdz4Cn62zGm46EGdyp5Po"
AUTH="A6OyHKDbTjFEDHDul9lyog"
SUB=$(curl -fsS -X POST "$BASE/api/v1/push/subscription" -H "Authorization: Bearer $BOB" \
  --data-urlencode "subscription[endpoint]=https://push.example/sub7" \
  --data-urlencode "subscription[keys][p256dh]=$P256" \
  --data-urlencode "subscription[keys][auth]=$AUTH" \
  --data-urlencode "data[policy]=all")
echo "$SUB" | python3 -c "import sys,json; s=json.load(sys.stdin); assert s['endpoint']=='https://push.example/sub7', s"
curl -fsS "$BASE/api/v1/push/subscription" -H "Authorization: Bearer $BOB" | python3 -c "import sys,json; assert 'server_key' in json.load(sys.stdin)"
curl -fsS -X DELETE "$BASE/api/v1/push/subscription" -H "Authorization: Bearer $BOB" >/dev/null
echo "push: OK"

# --- stray rows ---------------------------------------------------------------
# Unknown card URL -> pinned 404 (upstream `find_by!`); the JSON body
# proves the route is wired rather than absent.
body=$(curl -s -w '\n%{http_code}' "$BASE/api/v1/timelines/link?url=https://example.com/x")
code=$(echo "$body" | tail -1)
[ "$code" = "404" ] || { echo "timelines/link: $code"; exit 1; }
echo "$body" | head -1 | grep -qi "not found" || { echo "timelines/link body"; exit 1; }
code=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/apps/verify_credentials" -H "Authorization: Bearer $ALICE")
[ "$code" = "200" ] || { echo "verify_credentials: $code"; exit 1; }
for path in blocks mutes favourites bookmarks followed_tags featured_tags; do
  code=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/$path" -H "Authorization: Bearer $ALICE")
  [ "$code" = "200" ] || { echo "$path: $code"; exit 1; }
done
echo "stray rows: OK"

# --- search -------------------------------------------------------------------
curl -fsS -X POST "$BASE/api/v1/statuses" -H "Authorization: Bearer $ALICE" \
  --data-urlencode "status=quokka brunch zzzsmoke7" --data-urlencode "visibility=public" >/dev/null
curl -fsS "$BASE/api/v2/search?q=zzzsmoke7&type=statuses" -H "Authorization: Bearer $ALICE" \
  | python3 -c "import sys,json; r=json.load(sys.stdin); assert len(r['statuses'])==1, r"
curl -fsS "$BASE/api/v2/search?q=zzzsmoke7&type=statuses" \
  | python3 -c "import sys,json; r=json.load(sys.stdin); assert r['statuses']==[], r"
echo "search: OK"

# --- media lifecycle (no-transcode build: legacy inline finish) ----------------
python3 - <<'PYEOF'
import struct, zlib
def chunk(t, d):
    c = struct.pack('>I', len(d)) + t + d
    return c + struct.pack('>I', zlib.crc32(t + d) & 0xffffffff)
png = (b'\x89PNG\r\n\x1a\n'
       + chunk(b'IHDR', struct.pack('>IIBBBBB', 1, 1, 8, 2, 0, 0, 0))
       + chunk(b'IDAT', zlib.compress(b'\x00\x00\x00\x00'))
       + chunk(b'IEND', b''))
open('/tmp/smoke7.png', 'wb').write(png)
PYEOF
curl -fsS -X POST "$BASE/api/v1/media" -H "Authorization: Bearer $ALICE" \
  -F "file=@/tmp/smoke7.png;type=image/png" \
  | python3 -c "import sys,json; m=json.load(sys.stdin); assert m['type']=='image', m; assert isinstance(m['url'], str), m"
rm -f /tmp/smoke7.png
echo "media: OK"

# --- web bundle unconfigured -> 404 -------------------------------------------
code=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/app")
[ "$code" = "404" ] || { echo "bundle 404: $code"; exit 1; }
echo "SMOKE_PHASE7_OK"
