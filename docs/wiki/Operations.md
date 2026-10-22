# Operations

Running birchtree in production: one Rust process, one `redb` file, local media on disk, in-process jobs + streaming + search. No Redis, Postgres, Node, broker, or cluster mode.

## 1. Requirements

Build: stable Rust; gates `cargo check --all-targets` + `cargo test`, full gate `scripts/smoke-all.sh`. `ffmpeg` (external, not vendored) required whenever `transcoding_enabled` is true (default): startup proves `ffmpeg -version` + encoders `libvpx-vp9, libopus, libwebp` + 17 decoders including png/mjpeg/gif/bmp/webp/tiff/h264/hevc/mpeg4/vp8/vp9/audio set; missing = startup error, never silent passthrough. Disable only with `--no-transcode`. Known gap: `image/bmp` accepted but `bmp` not in `REQUIRED_DECODERS` — startup passes, BMP transcodes fail (add pin or drop BMP). Optional: SMTP provider (`smtp_url` + `mail_from`; half-configured = startup error, unconfigured = explicit 503); VAPID keypair for push; keystodon bundle dir (`web_client_dir`; unset = `/app/*` 404); extra TLS roots for lab federation.

## 2. Ports, paths, routes

Listen `--listen=` (default `0.0.0.0:3000`); identity `--public-url=` (trailing slash stripped; actor/object URIs derive from it). Single process serves: REST `/api/v1/*`, `/api/v2/*`, OAuth (`/api/v1/apps`, `/oauth/authorize|token|revoke|userinfo`), ActivityPub (`/inbox`, `/users/:username/inbox`, actor/collections/outbox, WebFinger, NodeInfo), media (`/system/media_attachments/files/*path`, `/system/custom_emojis/images/*path`), streaming (SSE `GET /api/v1/streaming/:channel`, WS any path under `/api/v1/streaming`, `GET /api/v1/streaming/health`), first-party `/api/web/*`, bundle `/app/*`. Ops: `GET /health` (liveness + storage snapshot), `GET /ready` (storage + probes, 2s deadline; failing names logged not returned), `GET /metrics` (loopback-only Prometheus; full ops surface is Phase-8 debt). Streaming host: `--streaming-url=` advertised as `urls.streaming`; unset = serve in-process (D16); set = 301 elsewhere.

## 3. Data-dir layout

```
<data_dir>/                        # --data-dir= (default ./data)
  birchtree.redb                   # the database (SCHEMA_VERSION 21)
  media/                           # durable pool: local uploads
  media_cache/                     # cache pool: remote bytes (derived, refetchable)
  tmp/{rand}.part                  # upload spool + ffmpeg / fetch staging
```

Backups: stop or snapshot; copy `birchtree.redb` + `media/` (+ `media_cache/` optional). Back up the secret keyring file/env **separately** (losing every sealing key orphans MFA seeds, actor keys, outbox bearers). Restore: put files at the same `data_dir` (or point `--data-dir=`), keep `durable_dir`/`cache_dir` disjoint, restart. Remote-cache bytes are per-row (no sharing); evicted entries refetch while the origin lives. Blob roots `0700`; keyring must not be group/other-readable.

## 4. Logging, metrics, proxies

`tracing`, default `info`, `RUST_LOG` override. Request IDs, body/upload caps, per-endpoint rate limits. Secret handling: `Bearer [redacted]`, startup summary carries only `configured` booleans + scheme+host SMTP endpoint — never tokens, codes, passwords, bodies, signatures, keys, seeds. Job mail-token payloads redacted. Client IP = TCP peer + `X-Forwarded-For` only when the peer is a configured `--trusted-proxy` (repeatable); rightmost-untrusted chain; untrusted peer's header ignored (no spoofing).

## 5. Resource caps and tuning

Upload/body: `max_file_size` = `max_body_size` = 16 MiB defaults (single cap for images+video+audio — no separate video knob; raise body before file). Multipart spool: 64 KiB text fields; status text default 500 / max 3000. Inbox 1 MiB (D27); remote docs 200 KiB; oEmbed 64 KiB; remote media streaming cap 40 MiB; relay bodies 1 MiB. Timeline scans: home-author 1000, account-cache 2000 (fail-closed), list-member 500, public/hashtag 2000, home/list merge 5000/600, multi-tag 4. Streaming bounds (D14): 512 total → 503; 16/token → 429; 16 subs/WS → 429 frame; 256 frames / 1 MiB queued → close; event queue 4096 overflow → close all; 30s stall → close; SSE 15s; WS ping 30s; retraction 64×500. Delivery (D49): 5 attempts/msg (1s→5min backoff), per-host budget 10, cooldown 60s doubling cap 1h, 25 deferrals → quarantine (tail ≈ 1d). Jobs: lease 30s, backoff 1s→300s, payloads < 64 KiB, batches ≤ 500, worker batch 32. Transcode: wall 120s, output 64 MiB, RSS 1 GiB; quality `webp -quality 80`, VP9 `-crf 30 -cpu-used 2`, Opus 96k. Threads: parent hops 40, quote-reverify hop 8 / budget 100, crawl 500/collection + 500 pages + 1000 total, ancestor walk 40, initial wait 5m + cooldown 15m, Move follower cap 10k, emoji 50/object + 256 KiB, scheduled 300 total / 25 per UTC day / 5-min floor. Search `embedded` (default) / `database` / `off` — deploy-time switch + restart (D6). Cache budgets: global 512 MiB, avatar/emoji 128 MiB, preview 256 MiB, full 128 MiB, avatar retention 30d, on-demand + no warming by default. Registration: closed default; approval queue; min age 16 (13–21). Production refuses weakened settings unless `--lab` or the exact override phrase.

## 6. Failure modes

ffmpeg missing with transcoding on = startup error; vanished-at-runtime = job retry → quarantine (never silent passthrough). Deterministic input failures (bad decode, ffmpeg non-zero, cap breach) commit terminal `Failed` (422 on read); environmental failures retry → quarantine; cooldown parks hot hosts without blocking others. Crashed transcodes leave `birchtree-transcode-*` tmp files (SIGKILL defeats guards); `sweep_tmp_files()` at startup + `sweep-transcode-orphans` CLI remove them; staging blobs overwritten by retries, swept per-media once terminal. Evicted cache 404s then single-flight refetches; evicted previews rebuild from the cached original. Crash protocol: parse + remote lookups before txn; one short `begin_write` (records+indexes+counters+dedup/jobs); media/streaming/federation after commit via idempotent workers; lease expiry re-drives orphans; `ACTIVITY_DEDUP` 7-day TTL + record-applied-only makes redelivery safe. Streaming overload closes slow clients (never buffered backlog); blocks/mutes retract backlog entries then bump the epoch. Mail down: sends 503 unconfigured, retry-then-quarantine when configured-but-failing; resends already answered `200 {}` (at-least-once: crash retry can duplicate).

## 7. Security notes

SSRF: every outbound fetch goes through the safe client (https-only unless lab-allowed, no creds, IP literals judged directly, blocked ranges incl. `10/8, 127/8, 169.254/16, 172.16/12, 192.168/16, ::1, fe80::/10, fc00::/7`, mapped/NAT64/6to4, `--federation-allow-private` allowlist; DNS re-validated per hop, redirect re-validated, connect 5s / request 10s, 5 redirect hops max). Signatures: HTTP-Signature create/verify + `Digest` on inbox (missing → 401); stale keys force-refetch once (60s cooldown); RSA-2048, `keyId={actor}#main-key`, Cavage legacy only (no Ed25519/RFC-9421 — modern-only senders 401); instance keypair on first start; per-account single-flight; private keys sealed (fail closed, never silently re-generated); authorized fetch gates note/quote-auth/collection reads when enabled. Tokens stored as `sha256_hex`; sessions as hashed cookies; TOTP seeds + signing keys + mail bearers AES-256-GCM sealed (production refuses without keyring; lab warns plaintext). Backup codes single-use hashed; WebAuthn challenges durable; password-reset 6h bearers consume-on-write (unknown/used indistinguishable 422); always-200 forgot-password (unknown queues nothing). Suspensions/disablements permanently revoke tokens and close streams (re-enable never restores). Open debt: Prometheus ops endpoint, Phase-8 perf, contract freeze, live interop proofs, keystodon smoke.
