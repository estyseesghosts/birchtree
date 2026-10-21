# 2026-10-21 — container rig + seeded testbench (birchtree side)

## What changed
- Transcribed `Dockerfile` (both targets), `.dockerignore`,
  `docker/birchtree.container.json`,
  `docker/birchtree.container.no-ffmpeg.json` from
  `birchtree_container.md` into the repo (new files, uncommitted).
- Built `birchtree:latest` (ffmpeg) and rebuilt `birchtree:no-ffmpeg`
  (the pre-existing image lacked the baked `/etc/birchtree/config.json`;
  first boot failed with `configuration error: --config
  /etc/birchtree/config.json: No such file or directory`). Rig runs the
  no-ffmpeg variant with `--no-transcode`.
- No source changes were needed (health/ready pre-existed).

## Topology
Same Model C as the pinetree log: `--network=host`, bt-a
`http://127.10.0.21:8080` … bt-d `http://127.10.0.24:8083`, identities
`uN@127.10.0.2x` (port stripped by `host_from_url` — kept as-is).
`RUST_LOG=info,birchtree::federation=debug,birchtree::jobs::worker=debug,activitypub_federation=debug`;
`--federation-allow-http --federation-allow-private=127.0.0.0/8,172.29.0.0/24`.

## Verified (real output)
- `/health` 200 on all bt nodes; startup log shows the lab-federation
  policy warning + weakened rate limits (x10) as intended.
- `podman logs bt-a | grep -c birchtree::federation` → 2234, e.g. inbound
  `Follow ... outcome=applied` with the outbound Accept `delivered (2xx)`.
- bt→pt lookup `u0@127.10.0.11:3000` → 200 with account id (port-ful pt
  handles resolve natively).
- bt→bt handle resolution cannot use bare handles (WebFinger → hijacked
  :80), so the seeder bridges via status-URI search (`/api/v2/search
  ?q=<uri>&resolve=true`, verified: returns the remote status and stores
  its author), then `lookup u@ip:port` → 200 (remote rows keep the
  authority port). bt mesh: 768 cross-node + 224 same-node (port-less
  local handles) follows, all OK.
- Seed totals (bt side): 128 posts + 32 second-wave, 64 photos, 32 videos,
  32 polls, 32 replies, 32 reblogs, 64 quotes, 32 favourites (+ poll votes).
- bt-a home timeline (100-status sample) contains pt-family posts and all
  bt nodes.
- Keystone in akkoma mode against bt-a serves :5174 and proxies API
  (app registration through the dev proxy → 200 with client_id).

## Known gaps (not regressions)
- `PATCH /api/v1/accounts/update_credentials` accepts urlencoded
  display_name/note only; avatar/header need the media pipeline (named
  follow-up in the code). Seeded bt accounts have unique display names +
  bios but default avatars. Pinetree accounts got unique uploaded avatars
  + banners (192 generated images).
- One stray test page `seed-x` remains on pt-a (`pages/delete` with a name
  400s; needs the page id).

## Follow-up: OAuth pages served as text/html (same day)
- Symptom (found via Keystone PKCE testing): `GET /oauth/authorize` answered
  `200 text/plain`, so browsers showed the sign-in form as source.
- Cause: `(StatusCode, String).into_response()` — axum defaults `String`
  bodies to `text/plain`.
- Fix: `login_page`/`consent_page`/`error_page` in
  `src/api/oauth/consent_page.rs` now return `Html<String>`.
- Verified: `cargo check --all-targets --locked` green (44s); rebuilt
  `birchtree:no-ffmpeg`; recreated bt-a..d on existing volumes (no reseed);
  `/oauth/authorize` now answers `200 text/html; charset=utf-8`.
