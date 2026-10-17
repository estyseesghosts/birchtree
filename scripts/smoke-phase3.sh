#!/usr/bin/env bash
# Phase 3 smoke: drive the core local social loop against the real binary with
# curl — statuses (create/show/edit/history/source/context/delete), reblogs,
# favourites/bookmarks, follows + follow requests, blocks/mutes/notes,
# endorsements, pins, polls, quotes + interaction policy, multi-fetch,
# visibility boundaries, domain blocks and idempotent retries.
# Ends with SMOKE_PHASE3_OK.
set -euo pipefail

cd "$(dirname "$0")/.."

PORT="${BIRCHTREE_SMOKE_PORT:-38919}"
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

# Extract a (possibly dotted) field from a JSON document on stdin.
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

# Fail unless the JSON path (dot/bracket notation via jq-free python) equals.
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
    elif part.endswith("]"):
        key, idx = part[:-1].split("[")
        cur = cur[key][int(idx)]
    else:
        cur = cur[part]
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

# Fail unless the JSON path (dot/bracket notation via jq-free python) equals.
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
    elif part.endswith("]"):
        key, idx = part[:-1].split("[")
        cur = cur[key][int(idx)]
    else:
        cur = cur[part]
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

# status_code method path token [data...]
code() {
  local method="$1" path="$2" token="${3:-}"; shift 3 || true
  local args=(-s -o /dev/null -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(-H "Authorization: Bearer $token")
  [ $# -gt 0 ] && args+=(--data-urlencode "$1") && shift && args+=("$@")
  curl "${args[@]}"
}

cargo build --bin birchtree >/dev/null

echo "==> start server"
"$BIN" --data-dir="$DATA_DIR" --listen="127.0.0.1:${PORT}" --public-url="$BASE" --registration-open &
PID=$!
for _ in $(seq 1 100); do
  curl -fsS "$BASE/health" >/dev/null 2>&1 && break
  sleep 0.1
done

echo "==> setup: app + three accounts"
ALL_SCOPES="read write follow profile read:accounts write:accounts read:follows write:follows read:statuses write:statuses read:favourites write:favourites read:bookmarks write:bookmarks read:blocks write:blocks read:mutes write:mutes read:notifications write:notifications"
APP_JSON="$(curl -fsS -X POST "$BASE/api/v1/apps" \
  --data-urlencode "client_name=Phase3 Smoke" \
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
ALICE="$(register alice)"
BOB="$(register bob)"
CAROL="$(register carol)"
[ -n "$ALICE" ] && [ -n "$BOB" ] && [ -n "$CAROL" ]
ALICE_ID="$(curl -fsS "$BASE/api/v1/accounts/verify_credentials" -H "Authorization: Bearer $ALICE" | json_get id)"
BOB_ID="$(curl -fsS "$BASE/api/v1/accounts/verify_credentials" -H "Authorization: Bearer $BOB" | json_get id)"
CAROL_ID="$(curl -fsS "$BASE/api/v1/accounts/verify_credentials" -H "Authorization: Bearer $CAROL" | json_get id)"

echo "==> create a status (alice)"
STATUS="$(curl -fsS -X POST "$BASE/api/v1/statuses" -H "Authorization: Bearer $ALICE" \
  --data-urlencode "status=hello #world from @bob" \
  --data-urlencode "visibility=public" \
  --data-urlencode "language=en")"
STATUS_ID="$(printf '%s' "$STATUS" | json_get id)"
[ -n "$STATUS_ID" ]
jassert "$STATUS" "content" "<p>hello #world from @bob</p>" "rendered content"
jassert "$STATUS" "visibility" "public" "visibility"
jassert "$STATUS" "favourites_count" "0" "fresh counts"
jassert "$STATUS" "account.username" "alice" "author"
jassert "$STATUS" "tags.0.name" "world" "hashtag extracted"
jassert "$STATUS" "mentions.0.acct" "bob" "mention resolved"
jassert "$STATUS" "quote_approval.automatic.0" "public" "default quote policy"
jassert "$STATUS" "language" "en" "language"

echo "==> anonymous show + 404 shape"
jassert "$(curl -fsS "$BASE/api/v1/statuses/$STATUS_ID")" "id" "$STATUS_ID" "anon show"
body="$(curl -s "$BASE/api/v1/statuses/999999999999999999")"
echo "$body" | grep -q '"error":"Not Found"' || { echo "404 shape: $body" >&2; exit 1; }

echo "==> favourite is idempotent"
jassert "$(curl -fsS -X POST "$BASE/api/v1/statuses/$STATUS_ID/favourite" -H "Authorization: Bearer $BOB")" "favourited" "true" "favourited"
jassert "$(curl -fsS -X POST "$BASE/api/v1/statuses/$STATUS_ID/favourite" -H "Authorization: Bearer $BOB")" "favourites_count" "1" "count stays 1 after retry"
jassert "$(curl -fsS -X POST "$BASE/api/v1/statuses/$STATUS_ID/unfavourite" -H "Authorization: Bearer $BOB")" "favourites_count" "0" "unfavourite"

echo "==> bookmark + reblog"
jassert "$(curl -fsS -X POST "$BASE/api/v1/statuses/$STATUS_ID/bookmark" -H "Authorization: Bearer $BOB")" "bookmarked" "true" "bookmarked"
REBLOG="$(curl -fsS -X POST "$BASE/api/v1/statuses/$STATUS_ID/reblog" -H "Authorization: Bearer $BOB")"
REBLOG_ID="$(printf '%s' "$REBLOG" | json_get id)"
jassert "$REBLOG" "reblog.id" "$STATUS_ID" "reblog wraps original"
jassert "$(curl -fsS "$BASE/api/v1/statuses/$STATUS_ID")" "reblogs_count" "1" "reblogs_count"
jassert "$(curl -fsS -X POST "$BASE/api/v1/statuses/$STATUS_ID/unreblog" -H "Authorization: Bearer $BOB")" "reblogs_count" "0" "unreblog"

echo "==> reply + context"
REPLY="$(curl -fsS -X POST "$BASE/api/v1/statuses" -H "Authorization: Bearer $BOB" \
  --data-urlencode "status=@alice hi!" \
  --data-urlencode "in_reply_to_id=$STATUS_ID")"
REPLY_ID="$(printf '%s' "$REPLY" | json_get id)"
jassert "$(curl -fsS "$BASE/api/v1/statuses/$STATUS_ID")" "replies_count" "1" "replies_count"
REPLY_CTX="$(curl -fsS "$BASE/api/v1/statuses/$REPLY_ID/context" -H "Authorization: Bearer $BOB")"
jassert "$REPLY_CTX" "ancestors.0.id" "$STATUS_ID" "reply sees parent as ancestor"
ROOT_CTX="$(curl -fsS "$BASE/api/v1/statuses/$STATUS_ID/context" -H "Authorization: Bearer $BOB")"
jassert "$ROOT_CTX" "descendants.0.id" "$REPLY_ID" "root sees reply as descendant"

echo "==> Idempotency-Key replays the created status"
KEY_JSON1="$(curl -fsS -X POST "$BASE/api/v1/statuses" -H "Authorization: Bearer $ALICE" -H "Idempotency-Key: retry-42" \
  --data-urlencode "status=retryable post")"
KEY_JSON2="$(curl -fsS -X POST "$BASE/api/v1/statuses" -H "Authorization: Bearer $ALICE" -H "Idempotency-Key: retry-42" \
  --data-urlencode "status=retryable post with different text")"
[ "$(printf '%s' "$KEY_JSON1" | json_get id)" = "$(printf '%s' "$KEY_JSON2" | json_get id)" ]

echo "==> multi-fetch + over-limit"
MULTI="$(curl -fsS "$BASE/api/v1/statuses?id[]=$STATUS_ID&id[]=$REPLY_ID" -H "Authorization: Bearer $ALICE")"
echo "$MULTI" | python3 -c 'import sys,json; a=json.load(sys.stdin); assert len(a)==2 and a[0]["id"]>a[1]["id"], a'
args=(-s -o /dev/null -w '%{http_code}' "$BASE/api/v1/statuses" -H "Authorization: Bearer $ALICE")
for i in $(seq 1 21); do args+=(--data-urlencode "id[]=$i"); done
[ "$(curl "${args[@]}")" = "422" ]

echo "==> edit + history + source"
curl -fsS -X PUT "$BASE/api/v1/statuses/$STATUS_ID" -H "Authorization: Bearer $ALICE" \
  --data-urlencode "status=hello #world from @bob (edited)" >/dev/null
EDITED="$(curl -fsS "$BASE/api/v1/statuses/$STATUS_ID")"
[ "$(printf '%s' "$EDITED" | json_get edited_at)" != "null" ]
HISTORY="$(curl -fsS "$BASE/api/v1/statuses/$STATUS_ID/history" -H "Authorization: Bearer $ALICE")"
echo "$HISTORY" | python3 -c 'import sys,json; h=json.load(sys.stdin); assert len(h)>=2, h'
SOURCE="$(curl -fsS "$BASE/api/v1/statuses/$STATUS_ID/source" -H "Authorization: Bearer $ALICE")"
jassert "$SOURCE" "text" "hello #world from @bob (edited)" "raw source"

echo "==> follow loop (bob -> alice) with real relationship flags"
jassert "$(curl -fsS -X POST "$BASE/api/v1/accounts/$ALICE_ID/follow" -H "Authorization: Bearer $BOB")" "following" "true" "bob follows alice"
jassert "$(curl -fsS -X POST "$BASE/api/v1/accounts/$ALICE_ID/follow" -H "Authorization: Bearer $BOB" --data-urlencode "reblogs=false" --data-urlencode "notify=true")" "showing_reblogs" "false" "show_reblogs honored"
jassert "$(curl -fsS -X POST "$BASE/api/v1/accounts/$ALICE_ID/follow" -H "Authorization: Bearer $BOB" --data-urlencode "reblogs=true" --data-urlencode "notify=false")" "showing_reblogs" "true" "restored"
REL="$(curl -fsS "$BASE/api/v1/accounts/relationships?id[]=$ALICE_ID&id[]=$CAROL_ID" -H "Authorization: Bearer $BOB")"
jassert "$REL" "0.following" "true" "relationship following"
jassert "$REL" "0.followed_by" "false" "alice does not follow bob yet"
jassert "$REL" "1.showing_reblogs" "false" "neutral pair defaults false"
FOLLOWERS="$(curl -fsS "$BASE/api/v1/accounts/$ALICE_ID/followers" -H "Authorization: Bearer $ALICE")"
echo "$FOLLOWERS" | python3 -c 'import sys,json; a=json.load(sys.stdin); assert any(x["username"]=="bob" for x in a), a'
[ "$(curl -fsS "$BASE/api/v1/accounts/verify_credentials" -H "Authorization: Bearer $ALICE" | python3 -c 'import sys,json; print(json.load(sys.stdin)["followers_count"])')" = "1" ]

echo "==> locked account follow request"
curl -fsS -X PATCH "$BASE/api/v1/accounts/update_credentials" -H "Authorization: Bearer $CAROL" --data-urlencode "locked=true" >/dev/null
REQ="$(curl -fsS -X POST "$BASE/api/v1/accounts/$CAROL_ID/follow" -H "Authorization: Bearer $BOB")"
jassert "$REQ" "requested" "true" "follow request pending"
jassert "$REQ" "following" "false" "not yet following"
REQUESTS="$(curl -fsS "$BASE/api/v1/follow_requests" -H "Authorization: Bearer $CAROL")"
echo "$REQUESTS" | python3 -c 'import sys,json; a=json.load(sys.stdin); assert any(x["username"]=="bob" for x in a), a'
# Authorize answers with the RELATIONSHIP OF THE VIEWER (carol) TO THE
# REQUESTER (bob): `followed_by` becomes true (Mastodon renders the requester
# with current_account's presenter).
AUTH="$(curl -fsS -X POST "$BASE/api/v1/follow_requests/$BOB_ID/authorize" -H "Authorization: Bearer $CAROL")"
jassert "$AUTH" "followed_by" "true" "authorize converts to follow"

echo "==> note + endorsement + block/mute"
NOTE="$(curl -fsS -X POST "$BASE/api/v1/accounts/$ALICE_ID/note" -H "Authorization: Bearer $BOB" --data-urlencode "comment=my pal")"
jassert "$NOTE" "note" "my pal" "account note stored"
jassert "$(curl -fsS -X POST "$BASE/api/v1/accounts/$ALICE_ID/pin" -H "Authorization: Bearer $BOB")" "endorsed" "true" "endorse (bob follows alice)"
ENDORSED="$(curl -fsS "$BASE/api/v1/endorsements" -H "Authorization: Bearer $BOB")"
echo "$ENDORSED" | python3 -c 'import sys,json; a=json.load(sys.stdin); assert any(x["username"]=="alice" for x in a), a'
curl -fsS -X POST "$BASE/api/v1/accounts/$ALICE_ID/unpin" -H "Authorization: Bearer $BOB" >/dev/null
BLOCK="$(curl -fsS -X POST "$BASE/api/v1/accounts/$CAROL_ID/block" -H "Authorization: Bearer $BOB")"
jassert "$BLOCK" "blocking" "true" "block set"
[ "$(curl -fsS -X POST "$BASE/api/v1/accounts/$CAROL_ID/unblock" -H "Authorization: Bearer $BOB" | json_get blocking)" = "false" ]
MUTE="$(curl -fsS -X POST "$BASE/api/v1/accounts/$CAROL_ID/mute" -H "Authorization: Bearer $BOB" --data-urlencode "duration=3600" --data-urlencode "notifications=true")"
jassert "$MUTE" "muting" "true" "mute set"
[ "$(printf '%s' "$MUTE" | json_get muting_expires_at)" != "null" ]
jassert "$(curl -fsS -X POST "$BASE/api/v1/accounts/$CAROL_ID/unmute" -H "Authorization: Bearer $BOB")" "muting" "false" "unmute"

echo "==> visibility boundaries"
PRIV="$(curl -fsS -X POST "$BASE/api/v1/statuses" -H "Authorization: Bearer $ALICE" \
  --data-urlencode "status=followers only" --data-urlencode "visibility=private")"
PRIV_ID="$(printf '%s' "$PRIV" | json_get id)"
[ "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/statuses/$PRIV_ID")" = "404" ]
[ "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/statuses/$PRIV_ID" -H "Authorization: Bearer $BOB")" = "200" ]
[ "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/statuses/$PRIV_ID" -H "Authorization: Bearer $CAROL")" = "404" ]
DIRECT="$(curl -fsS -X POST "$BASE/api/v1/statuses" -H "Authorization: Bearer $ALICE" \
  --data-urlencode "status=hi @bob" --data-urlencode "visibility=direct")"
DIRECT_ID="$(printf '%s' "$DIRECT" | json_get id)"
[ "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/statuses/$DIRECT_ID" -H "Authorization: Bearer $BOB")" = "200" ]
[ "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/statuses/$DIRECT_ID" -H "Authorization: Bearer $CAROL")" = "404" ]

echo "==> pin + pinned list"
curl -fsS -X POST "$BASE/api/v1/statuses/$STATUS_ID/pin" -H "Authorization: Bearer $ALICE" >/dev/null
jassert "$(curl -fsS "$BASE/api/v1/statuses/$STATUS_ID" -H "Authorization: Bearer $ALICE")" "pinned" "true" "pinned flag"
PINNED="$(curl -fsS "$BASE/api/v1/accounts/$ALICE_ID/statuses?pinned=true" -H "Authorization: Bearer $ALICE")"
echo "$PINNED" | python3 -c 'import sys,json; a=json.load(sys.stdin); assert any(x["id"]=="'"$STATUS_ID"'" for x in a), a'
curl -fsS -X POST "$BASE/api/v1/statuses/$STATUS_ID/unpin" -H "Authorization: Bearer $ALICE" >/dev/null

echo "==> poll create + vote"
POLL="$(curl -fsS -X POST "$BASE/api/v1/statuses" -H "Authorization: Bearer $ALICE" \
  --data-urlencode "status=which one?" \
  --data-urlencode "poll[multiple]=false" \
  --data-urlencode "poll[hide_totals]=true" \
  --data-urlencode "poll[expires_in]=3600" \
  --data-urlencode "poll[options][]=tea" \
  --data-urlencode "poll[options][]=coffee")"
POLL_ID="$(printf '%s' "$POLL" | json_get poll.id)"
[ -n "$POLL_ID" ]
# Mastodon treats the poll AUTHOR as a voter without a ballot.
jassert "$POLL" "poll.voted" "true" "author counts as voted"
jassert "$POLL" "poll.voters_count" "0" "no ballots yet"
VOTE="$(curl -fsS -X POST "$BASE/api/v1/polls/$POLL_ID/votes" -H "Authorization: Bearer $BOB" --data-urlencode "choices[]=0")"
jassert "$VOTE" "voted" "true" "bob voted"
jassert "$VOTE" "own_votes.0" "0" "own_votes"
jassert "$VOTE" "options.1.votes_count" "null" "hide_totals masks option counts"
[ "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/api/v1/polls/$POLL_ID/votes" -H "Authorization: Bearer $BOB" --data-urlencode "choices[]=1")" = "422" ]

echo "==> quote create + revoke + interaction policy"
QUOTE="$(curl -fsS -X POST "$BASE/api/v1/statuses" -H "Authorization: Bearer $BOB" \
  --data-urlencode "status=quoting alice" \
  --data-urlencode "quoted_status_id=$STATUS_ID")"
QUOTE_ID="$(printf '%s' "$QUOTE" | json_get id)"
jassert "$QUOTE" "quote.state" "accepted" "local quote auto-accepted"
jassert "$QUOTE" "quote.quoted_status.id" "$STATUS_ID" "quoted content present"
jassert "$(curl -fsS "$BASE/api/v1/statuses/$STATUS_ID")" "quotes_count" "1" "quotes_count"
QUOTES="$(curl -fsS "$BASE/api/v1/statuses/$STATUS_ID/quotes" -H "Authorization: Bearer $ALICE")"
echo "$QUOTES" | python3 -c 'import sys,json; a=json.load(sys.stdin); assert len(a)==1 and a[0]["id"]=="'"$QUOTE_ID"'", a'
POLICY="$(curl -fsS -X PUT "$BASE/api/v1/statuses/$STATUS_ID/interaction_policy" -H "Authorization: Bearer $ALICE" \
  --data-urlencode "quote_approval_policy=nobody")"
jassert "$POLICY" "quote_approval.automatic" "[]" "policy nobody clears automatic"
REVOKED="$(curl -fsS -X POST "$BASE/api/v1/statuses/$STATUS_ID/quotes/$QUOTE_ID/revoke" -H "Authorization: Bearer $ALICE")"
jassert "$REVOKED" "quote.state" "revoked" "revoke state"

echo "==> domain blocks"
curl -fsS -X POST "$BASE/api/v1/domain_blocks" -H "Authorization: Bearer $BOB" --data-urlencode "domain=remote.example" >/dev/null
curl -fsS "$BASE/api/v1/domain_blocks" -H "Authorization: Bearer $BOB" | python3 -c 'import sys,json; a=json.load(sys.stdin); assert a==["remote.example"], a'
curl -fsS -X DELETE "$BASE/api/v1/domain_blocks?domain=remote.example" -H "Authorization: Bearer $BOB" >/dev/null
[ "$(curl -fsS "$BASE/api/v1/domain_blocks" -H "Authorization: Bearer $BOB")" = "[]" ]

echo "==> delete returns source and 404s afterwards"
DELETED="$(curl -fsS -X POST "$BASE/api/v1/statuses" -H "Authorization: Bearer $BOB" --data-urlencode "status=temp")"
DELETED_ID="$(printf '%s' "$DELETED" | json_get id)"
DEL_BODY="$(curl -fsS -X DELETE "$BASE/api/v1/statuses/$DELETED_ID" -H "Authorization: Bearer $BOB")"
echo "$DEL_BODY" | grep -q '"text":"temp"' || { echo "delete body not source mode: $DEL_BODY" >&2; exit 1; }
echo "$DEL_BODY" | grep -q '"content"' && { echo "delete body leaked content" >&2; exit 1; }
[ "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/statuses/$DELETED_ID")" = "404" ]

echo "==> graceful shutdown"
kill -TERM "$PID"; wait "$PID" 2>/dev/null || true; PID=""

echo "SMOKE_PHASE3_OK"
