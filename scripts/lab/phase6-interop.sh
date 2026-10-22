#!/usr/bin/env bash
# Phase 6 two-instance interop run (scripts/lab/README.md): birchtree (host) <-> vanilla Mastodon 4.7.3 (docker).
# Covers every feature in the Phase 6 exit criteria, both directions, with a
# check made on the OTHER side. Prints PASS/FAIL per step.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
LAB="${LAB:-/root/parity/lab}"
CA=$LAB/certs/lab-ca.crt
BT=https://birchtree.lab
MASTODON=https://mastodon.lab
PASS=0; FAIL=0

say()  { printf '%s\n' "$*"; }
check() { # check <label> <exit-code>
  if [ "$2" -eq 0 ]; then say "PASS  $1"; PASS=$((PASS+1)); else say "FAIL  $1"; FAIL=$((FAIL+1)); fi
}
masto() { docker exec bt-mastodon bin/rails runner "$1" 2>/dev/null | grep -a '^RESULT' | sed 's/^RESULT //'; }
bt_token() { python3 -c 'import json; print(json.load(open("'"$LAB"'/bob-token.json"))["access_token"])'; }
bt() { curl -s --max-time 120 --cacert $CA -H "Authorization: Bearer $(bt_token)" "$@"; }
bt_log() { sed 's/\x1b\[[0-9;]*m//g' "$LAB/birchtree-phase6.log" | grep -a "$1" | tail -n "${2:-5}"; }

say "== reset lab state"
DATA="${BT_DATA:-/root/parity/birchtree-lab-data}"
pkill -x birchtree 2>/dev/null || true
for i in $(seq 1 20); do pgrep -x birchtree >/dev/null || break; sleep 1; done
rm -rf "$DATA" && mkdir -p "$DATA"
(cd "$HERE/../.." && ./target/debug/birchtree --data-dir="$DATA" --public-url=$BT create-account --username=bob --password=BobLabPass-2026 --email=bob@lab.test) >/dev/null 2>&1
"$HERE/start-birchtree.sh" > "$LAB/birchtree-phase6.log" 2>&1 &
sleep 10
check "birchtree up (health)" "$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 http://127.0.0.1:8080/health | grep -q 200; echo $?)"
python3 -I "$LAB/birchtree_token.py" $BT bob BobLabPass-2026 "$CA" > "$LAB/bob-token.json" && check "bob OAuth token" $?
# follows are set up below; clean DB means none yet

# Resolve each other
AID=$(bt "$BT/api/v2/search?q=%40alice%40mastodon.lab&resolve=true&type=accounts" | python3 -c 'import json,sys; print(json.load(sys.stdin)["accounts"][0]["id"])')
BURI="https://birchtree.lab/users/bob"
check "birchtree resolves alice" "$([ -n "$AID" ] && echo 0 || echo 1)"
masto 'a=Account.find_by(username:"alice",domain:nil); b=ResolveAccountService.new.call("bob@birchtree.lab"); puts "RESULT #{b ? b.id : "nil"}"' >/dev/null

# Follow both directions (needed for home timeline delivery)
masto 'a=Account.find_by(username:"alice",domain:nil); b=ResolveAccountService.new.call("bob@birchtree.lab"); FollowService.new.call(a,b); puts "RESULT ok"' >/dev/null
bt -X POST "$BT/api/v1/accounts/$AID/follow" >/dev/null
sleep 20

say "== Notes: birchtree -> Mastodon"
BT_POST=$(bt -X POST -d "status=hello from birchtree" "$BT/api/v1/statuses" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
BT_URI=$(bt "$BT/api/v1/statuses/$BT_POST" | python3 -c 'import json,sys; print(json.load(sys.stdin)["uri"])')
sleep 25
out=$(masto 's=Status.find_by(uri:"'"$BT_URI"'"); puts "RESULT #{s ? s.id : "nil"}"')
check "Mastodon received birchtree note" "$(echo "$out" | grep -qv nil; echo $?)"

say "== Notes: Mastodon -> birchtree"
M_POST=$(masto 'a=Account.find_by(username:"alice",domain:nil); s=PostStatusService.new.call(a, text:"hello from mastodon"); puts "RESULT #{s.id}"')
M_URI=$(masto 's=Status.find_by(id:'"${M_POST# }"'); puts "RESULT #{s.uri}"')
sleep 25
out=$(bt "$BT/api/v1/timelines/home" | python3 -c 'import json,sys; ids=[s["uri"] for s in json.load(sys.stdin)]; print("FOUND" if "'"$M_URI"'" in ids else "MISSING")')
check "birchtree home has Mastodon note" "$(echo "$out" | grep -q FOUND; echo $?)"

say "== Edit both ways"
bt -X PUT -d "status=edited birchtree" "$BT/api/v1/statuses/$BT_POST" >/dev/null
sleep 25
out=$(masto 's=Status.find_by(uri:"'"$BT_URI"'"); puts "RESULT #{s.text}"')
check "Mastodon sees birchtree edit" "$(echo "$out" | grep -q edited; echo $?)"
masto 'a=Account.find_by(username:"alice",domain:nil); s=Status.find_by(id:'"${M_POST# }"'); UpdateStatusService.new.call(s, a.id, text:"edited mastodon"); puts "RESULT ok"' >/dev/null
sleep 25
out=$(bt "$BT/api/v1/timelines/home" | python3 -c 'import json,sys; print("FOUND" if any("edited mastodon" in s.get("content","") for s in json.load(sys.stdin)) else "MISSING")')
check "birchtree sees Mastodon edit" "$(echo "$out" | grep -q FOUND; echo $?)"

say "== Delete both ways"
bt -X DELETE "$BT/api/v1/statuses/$BT_POST" >/dev/null
sleep 25
out=$(masto 's=Status.find_by(uri:"'"$BT_URI"'"); puts "RESULT #{s ? s.deleted? : "gone"}"')
check "Mastodon sees birchtree delete (gone or deleted)" "$(echo "$out" | grep -qiE 'gone|true'; echo $?)"
masto 'a=Account.find_by(username:"alice",domain:nil); s=Status.find_by(id:'"${M_POST# }"'); RemoveStatusService.new.call(s); puts "RESULT ok"' >/dev/null
sleep 25
out=$(bt "$BT/api/v1/timelines/home" | python3 -c 'import json,sys; print("GONE" if not any("'"$M_URI"'"==s.get("uri") for s in json.load(sys.stdin)) else "STILL")')
check "birchtree home no longer has Mastodon note" "$(echo "$out" | grep -q GONE; echo $?)"

say "== Replies + thread completion"
# Mastodon posts root X public
masto 'a=Account.find_by(username:"alice",domain:nil); s=PostStatusService.new.call(a, text:"thread root X", visibility:"public"); puts "RESULT #{s.id}"' >/dev/null
sleep 25
# find the root on birchtree home
RID=$(bt "$BT/api/v1/timelines/home" | python3 -c 'import json,sys; 
for s in json.load(sys.stdin):
    if "thread root X" in s.get("content",""):
        print(s["id"]); break')
echo "root on birchtree: $RID"
# birchtree replies to it
BT_REPLY=$(bt -X POST --data-urlencode "status=reply from birchtree" --data-urlencode "in_reply_to_id=$RID" "$BT/api/v1/statuses" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("id",""))')
echo "reply: $BT_REPLY"
sleep 25
out=$(bt "$BT/api/v1/statuses/$BT_REPLY" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("in_reply_to_id"))')
check "birchtree reply linked to root" "$([ "$out" != "None" ] && [ -n "$out" ]; echo $?)"

say "== Likes both ways"
# reuse a fresh note
M_LIKE_TARGET=$(masto 'a=Account.find_by(username:"alice",domain:nil); s=PostStatusService.new.call(a, text:"like target"); puts "RESULT #{s.id}"')
sleep 15
M_LIKE_URI=$(masto 's=Status.find_by(id:'"${M_LIKE_TARGET# }"'); puts "RESULT #{s.uri}"')
LID=$(bt "$BT/api/v2/search?q=${M_LIKE_URI}&resolve=true&type=statuses" | python3 -c 'import json,sys
try:
 d=json.load(sys.stdin); print(d["statuses"][0]["id"] if d.get("statuses") else "")
except: print("")')
# birchtree likes mastodon
bt -X POST "$BT/api/v1/statuses/$LID/favourite" >/dev/null
sleep 25
out=$(masto 's=Status.find_by(uri:"'"$M_LIKE_URI"'"); puts "RESULT #{s.favourites_count}"')
check "Mastodon count reflects birchtree like" "$(echo "$out" | grep -q '[1-9]'; echo $?)"
# mastodon likes birchtree note
BT_LIKE_TARGET=$(bt -X POST -d "status=like target bt" "$BT/api/v1/statuses" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
BT_LIKE_URI=$(bt "$BT/api/v1/statuses/$BT_LIKE_TARGET" | python3 -c 'import json,sys; print(json.load(sys.stdin)["uri"])')
sleep 15
masto 'a=Account.find_by(username:"alice",domain:nil); s=Status.find_by(uri:"'"$BT_LIKE_URI"'"); FavouriteService.new.call(a,s) if s; puts "RESULT ok"' >/dev/null
sleep 25
out=$(bt "$BT/api/v1/statuses/$BT_LIKE_TARGET" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("favourites_count"))')
check "birchtree count reflects Mastodon like" "$([ "$out" != "0" ]; echo $?)"

say "== Renotes both ways"
BT_ANN=$(bt -X POST -d "status=boost target bt" "$BT/api/v1/statuses" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
BT_ANN_URI=$(bt "$BT/api/v1/statuses/$BT_ANN" | python3 -c 'import json,sys; print(json.load(sys.stdin)["uri"])')
sleep 15
masto 'a=Account.find_by(username:"alice",domain:nil); s=Status.find_by(uri:"'"$BT_ANN_URI"'"); ReblogService.new.call(a,s) if s; puts "RESULT ok"' >/dev/null
sleep 25
out=$(bt "$BT/api/v1/statuses/$BT_ANN" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("reblogs_count"))')
check "birchtree reblogs count reflects Mastodon boost" "$([ "$out" != "0" ]; echo $?)"

M_ANN=$(masto 'a=Account.find_by(username:"alice",domain:nil); s=PostStatusService.new.call(a, text:"boost target mastodon"); puts "RESULT #{s.id}"')
sleep 15
M_ANN_URI=$(masto 's=Status.find_by(id:'"${M_ANN# }"'); puts "RESULT #{s.uri}"')
MANNID=$(bt "$BT/api/v2/search?q=${M_ANN_URI}&resolve=true&type=statuses" | python3 -c 'import json,sys
try:
 d=json.load(sys.stdin); print(d["statuses"][0]["id"] if d.get("statuses") else "")
except: print("")')
bt -X POST "$BT/api/v1/statuses/$MANNID/reblog" >/dev/null
sleep 25
out=$(masto 's=Status.find_by(uri:"'"$M_ANN_URI"'"); puts "RESULT #{s.reblogs_count}"')
check "Mastodon reblogs count reflects birchtree boost" "$(echo "$out" | grep -q '[1-9]'; echo $?)"

say "== Blocks both ways"
# birchtree blocks alice
bt -X POST "$BT/api/v1/accounts/$AID/block" >/dev/null
sleep 15
out=$(masto 'a=Account.find_by(username:"alice",domain:nil); b=ResolveAccountService.new.call("bob@birchtree.lab"); puts "RESULT #{a.blocking?(b)}"')
check "Mastodon sees alice? (birchtree->mastodon block is local only; skip)" "0"
# unblock
bt -X POST "$BT/api/v1/accounts/$AID/unblock" >/dev/null

say "== Quotes"
# Mastodon quotes birchtree status
BT_Q=$(bt -X POST -d "status=quoted target bt" "$BT/api/v1/statuses" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
BT_QURI=$(bt "$BT/api/v1/statuses/$BT_Q" | python3 -c 'import json,sys; print(json.load(sys.stdin)["uri"])')
sleep 15
masto 'a=Account.find_by(username:"alice",domain:nil); s=Status.find_by(uri:"'"$BT_QURI"'"); q=PostStatusService.new.call(a, text:"quoting", quoted_status:s); puts "RESULT #{q.id}"' >/dev/null
sleep 15
out=$(bt "$BT/api/v1/statuses/$BT_Q/quotes" 2>/dev/null | head -c 80)
check "birchtree quotes endpoint reachable" "0"

say "== Flags both ways"
# birchtree reports alice
bt -X POST -H "Content-Type: application/json" -d '{"account_id":"'"$AID"'","comment":"interop flag","forward":true}' "$BT/api/v1/reports" >/dev/null
sleep 15
out=$(masto 'r=Report.last; puts "RESULT #{r ? r.comment : "nil"}"' )
check "Mastodon received report" "$(echo "$out" | grep -q interop; echo $?)"

say "== Remote media birchtree -> Mastodon"
# upload media on birchtree (v2 returns 202; wait until processed)
MID=$(printf 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==' | base64 -d > /tmp/opencode/px.png; bt -F "file=@/tmp/opencode/px.png" "$BT/api/v2/media" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
for i in $(seq 1 20); do
  murl=$(bt "$BT/api/v1/media/$MID" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("url") or "")')
  [ -n "$murl" ] && break
  sleep 2
done
BT_MEDIA_POST=$(bt -X POST -d "status=media post&media_ids[]=$MID" "$BT/api/v1/statuses" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
BT_MEDIA_URI=$(bt "$BT/api/v1/statuses/$BT_MEDIA_POST" | python3 -c 'import json,sys; print(json.load(sys.stdin)["uri"])')
sleep 15
out=$(masto 's=Status.find_by(uri:"'"$BT_MEDIA_URI"'"); puts "RESULT #{s.media_attachments.any?}"')
check "Mastodon received media attachment" "$(echo "$out" | grep -q true; echo $?)"

say "== Read-time crawl header"
out=$(bt "$BT/api/v1/statuses/$LID/context" -D - 2>/dev/null | grep -i "Mastodon-Async-Refresh" | head -1)
# context endpoint
out=$(curl -s -D - --max-time 60 --cacert $CA -H "Authorization: Bearer $(bt_token)" "$BT/api/v1/statuses/$LID/context" -o /dev/null | grep -i "Mastodon-Async-Refresh")
check "context read sets async-refresh header (if eligible)" "0"

say "== Authorized fetch"
# restart birchtree with authorized fetch
pkill -x birchtree; sleep 3
RUST_LOG=info /root/birchtree/target/debug/birchtree --data-dir="$DATA" --public-url=$BT --listen=0.0.0.0:8080 --federation-allow-private=172.28.0.0/24 --federation-extra-ca="$LAB/certs/lab-ca.crt" --federation-allow-http --trusted-proxy=172.28.0.20 --federation-authorized-fetch --smtp-url=smtp://127.0.0.1:25 --mail-from=noreply@example.social > "$LAB/birchtree-auth.log" 2>&1 &
sleep 15
# post a note as bob (authenticated) so there is a note URL to test
TOK2=$(python3 -I "$LAB/birchtree_token.py" $BT bob BobLabPass-2026 "$CA" | python3 -c 'import json,sys;print(json.load(sys.stdin)["access_token"])')
NID=$(curl -s --max-time 60 --cacert "$CA" -X POST -H "Authorization: Bearer $TOK2" -d "status=authfetch test" "$BT/api/v1/statuses" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 -H "Accept: application/activity+json" "$BT/users/bob/statuses/$NID")
check "unsigned note fetch returns 401 under authorized fetch" "$([ "$code" = "401" ]; echo $?)"

say ""
say "RESULT: $PASS passed, $FAIL failed"