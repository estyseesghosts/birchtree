Conclusive: both bt→bt loops are birchtree bugs, different layers. Network ruled out.

Topology (`/root/bench/run3`, `20261019T021448Z`): bt-a/b/c `http://bt-*.lab:80` → `172.28.0.30` proxy → `172.28.0.1:8080/81/82`, flags `--federation-allow-http --federation-allow-private=127.0.0.0/8,172.28.0.0/24`. `Create/Like/Delete` arrive bt→bt `delivered (2xx)` by thousands — transport, signatures, routing fine.

### 1. Media 6/6 FAIL — inbound async race + missing `remote_url`

Receiver-side, deterministic `url=None status=exc`:

- `birchtree/src/application/federation_inbound.rs:127-138`: `ingest_note` creates `Pending` media row per attachment, enqueues `FetchRemoteMedia`, commits status immediately.
- `birchtree/src/federation/media.rs:23-101` → `persistence/redb/federation_content.rs:390-422` `stage_remote_media` → `ProcessMedia`. Only then `Ready` + URL. Logs prove fetch works: `FetchRemoteMedia … bytes=69 mime="image/png" outcome="staged"` (69B = `phase-features.py:45-54` fixture).
- `birchtree/src/api/mastodon/status_view.rs:230` loads all `media_by_ids` unfiltered. `birchtree/src/api/mastodon/dto.rs:501-523` `media_attachment_json`: `url=Null` unless `Ready`, `remote_url` hardcoded `Null` (line 517). Stored `REMOTE_MEDIA_SOURCE` never surfaced.
- vs Mastodon `/root/fedifedifedi/mastodon/app/serializers/rest/media_attachment_serializer.rb:25-33`: `url` nil when `not_processed?` but `remote_url=object.remote_url` immediately → harness fetches origin → PASS.
- vs pinetree `src/media/remote_cache.rs:111-150`: appends `file_ids` only *after* fetch, so `files` stays empty until ready and harness keeps polling.
- Harness `phase-features.py` `files_seen`: returns true on first attachment entry (still `Pending`), reads `url or remote_url or textUrl` → all null → `lib.py:544-552` `fetch_bytes(None)=exc`. Worker poll `src/config.rs:65` 250ms + 2 job hops ≈1-3s inbound-fetch lead — harness never waits 90s.
- Live proof: run-3 bt-b still up, akkoma/catodon-origin `117465367385735168`, `117465363786366976` now `Ready` with `url:http://bt-b.lab/system/.../remote/<id>`.

Secondary: `birchtree/src/federation/media.rs:83` + `src/application/media.rs:317-320`: all remote previews → `remote/small.png` collision. Local uploads use `new_shard()` `application/media.rs:42-77`.

### 2. Boost 6/6 FAIL — wrong-domain Announce `id` (outbound bug, only strict receivers reject)

- `birchtree/src/persistence/redb/statuses.rs:284-299` `reblog_links`: strips trailing `/{target_id}` off *target* URI, appends `/{reblog_id}`. Boosting `https://bt-a/users/alice/statuses/9` from bt-b → `https://bt-a/.../statuses/<reblog_id>` — bt-b minting ID on bt-a domain. Local shape `statuses.rs:338-339` is `{base}/users/{u}/statuses/{id}`.
- `birchtree/src/activitypub/outgoing.rs:320-334`: Announce `id=boost_uri`, `actor=booster`. So `id.host(origin) != actor.host(booster)`.
- Receipt: `birchtree/vendor/activitypub_federation/src/lib.rs:60` → `config.rs:126-142` `verify_url_and_domain` → `protocol/verification.rs:17-24` host compare → `UrlVerificationError`, plus `config.rs:135-139` “sent from local instance” if id on receiver domain. `birchtree/src/activitypub/error.rs:37,100` → 403. `vendor/.../activity_sending.rs:119-127` treats 4xx as delivered-abort, `src/federation/delivery.rs:142-149` logs `delivered (2xx)` even on 403. Sender silent, receiver drops before `activities.rs:995-1014` → `federation_inbound.rs:303-318`.
- Evidence: `nodes/bt-a.log:3855ff` ~130-140/node `403 URL failed verification: Domains do not match`, zero bare-Announce `delivered` to `bt-*.lab`, `announce without boost row:0` (planning OK). Outbound `activity=http://pt-a.test/notes/...` delivered 2xx to 6 non-bt inboxes.
- Why matrix matches: pinetree `src/federation/inbox.rs:276-278` returns true for Announce, `handle_announce:923-981` stores `uri=activity_id` regardless → bt→pt PASS. Mastodon lenient → PASS. `Like #likes/… (outgoing.rs:298-306)`, `Create {uri}/activity`, `Delete {uri}#delete` all actor-local → PASS. `undo_announce outgoing.rs:337-350` `{boost_uri}/undo` same defect → unboost vacuous PASS, would 403 if real.

Noise, not causes: `422 … untagged enum Activity` for quote Creates, `misskey→* stuck at 1` unboost (misskey-side).

### Fixes (for build agent)

1. Media: thread `REMOTE_MEDIA_SOURCE` through to `remote_url` in `dto.rs:501-523` (Mastodon parity, minimal), or exclude non-`Ready` like pinetree. Fix shared `remote/small.png` → per-media shard. Harness: `files_seen` require non-null `url/remote_url`.
2. Boost: mint wrapper `{local_base}/users/{booster}/statuses/{reblog_id}` in `reblog_status` — thread base/username in like `create_status statuses.rs:334-340` does. No migration (one-shot dedup keys).

Verify: fresh run `grep -c "FAIL.*media.*bt-" features.out==0`, `GET /statuses/:id` shows `remote_url` immediately + `url` in seconds; Announce body `id.host==actor.host`, 403 count →0, bt→bt renote 0→1→0 non-vacuous, bt→pt/masto unchanged + `cargo test`.

## Verification 2026-10-20 (docs worker W3, code read at HEAD, no re-run)

- **§2 Boost — FIXED.** `reblog_links` now mints the wrapper on the
  booster's actor URI (`src/persistence/redb/statuses.rs:288-292`, incl. the
  comment citing the domain-check refusal); `reblog_status` threads the
  booster's `actor_uri` through (`statuses.rs:1388-1400`); the delivery path
  sends `boost.uri` as the Announce `id`
  (`src/federation/events.rs:107-114`) via `outgoing::announce`
  (`src/activitypub/outgoing.rs:336-349`), so `id.host == actor.host`.
  No TODO filed.
- **§1 Media `remote_url` — still open, already tracked.** `REMOTE_MEDIA_SOURCE`
  is stored (`src/persistence/redb/federation_content.rs:455-469`) and the eager
  fetch path stages bytes to per-row `remote/{media_id}` keys
  (`src/federation/media.rs:23-102`, key at `:83`), but `media_attachment_json`
  still hardcodes `"remote_url": Null` (`src/api/mastodon/dto.rs:669-670`).
  Carried by `docs/TODOs.md` ("Remote media fidelity" row) — not double-filed
  here. Pending rows keep `url: null`, which matches the Mastodon
  `not_processed?` contract (`dto.rs:653-659`).
- **§1 secondary (shared `remote/small.png` collision) — gone.** Previews land
  under the per-media key (`media.rs:83`); `TODOs.md` records "per-media blob
  keys already shard correctly."
