# Handoff: Birchtree — media + media federation investigation and fixes

## Context (read first)

- Bench run5: `/root/pinetree/docs/federation-bench-2026-10-19-phase7-run5.md` (§ features media, § stress S4, "If put online today") + `/root/pinetree/docs/federation-bench-2026-10-19-run5-log.md`
- Bench evidence (all Mastodon-involved, zero non-Mastodon failures — determine what is ours vs theirs):
  - features media 18: `* -> mastodon` 9x (`pt/bt -> mastodon` 6x `mismatch:404`, `misskey/catodon/akkoma -> mastodon` 3x `no-attachment`); `mastodon -> *` 9x `mismatch:200` (re-encode). In-scope: 12.
  - stress S4: 155/202 verified (run4: 159/202). All 47 unverified mastodon-involved: 18x `no-attachment` (dst=mastodon) + 7x `mismatch:404` (dst=mastodon) + 22x `mismatch:200` (src=mastodon re-encode). `arrival_after_settle` max 6.1s.
  - harness: `/root/bench/` phase-features / phase-stress media checks; possible lab-CA gap (item 44) — verify.
- Birchtree media code (start here, verify line numbers in tree):
  - `src/media/` (`ingest.rs`, `processing.rs`, `blob_store.rs`, `remote_cache.rs`, `mod.rs`)
  - `src/federation/media.rs`, `src/federation/remote_media.rs`, `src/federation/delivery.rs`, `src/federation/net.rs` (timeouts, redirect/SSRF limits)
  - `src/activitypub/objects.rs`, `src/activitypub/wire.rs` (Document/attachment emit), `src/activitypub/outbound.rs`, `src/activitypub/outgoing.rs`
  - `src/domain/media.rs`, `src/persistence/redb/media.rs`
  - Mastodon reference (read-only): `/root/parity/mastodon-v4.7.3` (media ingest, `statuses_controller`, fetch/re-encode path)
- Docs: `birchtree/docs/` (federation-contract, compatibility-profile, phase exits), `birchtree.md`, `AGENTS.md` in `/root/birchtree`

## Objective

Investigate media upload + media federation end-to-end, then fix anything that is ours. Separate bench artifacts and Mastodon-side behavior from real Birchtree gaps.

## Instructions (use subagents)

You MUST use subagents for parallel investigation. Suggested split (run in parallel):

- Subagent A (outbound bt -> mastodon 404s): trace our attachment emit (Document shape, `url`/`mediaType`/`blurhash`, auth on media URLs, hostname exposure rule `https://{domain}/...` with no node hostnames, TLS/lab CA in bench). Reproduce `mismatch:404` with wire capture. Determine emit-side vs mastodon-fetch-side vs bench-CA.
- Subagent B (inbound mastodon -> bt re-encode mismatches): trace our remote fetch + cache + re-serve (do we proxy bytes, re-encode, or pass through? size limits, timeouts, content-type handling). Determine whether `mismatch:200` is expected (Mastodon re-encodes, bytes legitimately differ) and whether our verifier/harness comparison is too strict vs a real product gap.
- Subagent C (`no-attachment` when dst=mastodon): is our attachment missing on the wire, wrong shape for Mastodon's parser, or Mastodon dropping it on ingest? Compare pt/bt emit vs misskey/catodon/akkoma emit that fails identically — if three independent senders fail identically, lean mastodon-ingest-side but prove with payload diff.

Synthesize A+B+C before changing code. No fix without wire evidence.

## Questions to answer

- What exact Document/attachment JSON do we emit for 1-image, multi-image, video, GIF? What does Mastodon 4.7.3 require to ingest (field names, `mediaType` vs `mimeType`, `url` string vs object, `sensitive`, `blurhash`)?
- Are our media URLs publicly fetchable from the receiver (auth, signed URLs expiring, private-network blocking, lab CA, http vs https)? Check `net.rs` fetch path + nginx/tls in bench.
- Inbound: do we store original bytes + original content-type, or normalized? Size/timeout/redirect caps? What happens on Mastodon's re-encoded JPEG (different hash, same pixels) — should that count as PASS?
- Performance/correctness: S4 `arrival_after_settle` 6.1s — is that queue/delivery lag or fetch slowness? Concurrent media uploads p50 418ms — any failure under burst?

## Implementation rules

- Fix only what is proven ours with wire + code evidence (file:line both sides). If it is Mastodon-side or bench-CA/harness-strictness, document and do not contort our emit to satisfy a broken oracle.
- Keep architecture rules per birchtree AGENTS/proposal: domain has no HTTP/DB shapes; application ops shared by API + ActivityPub adapters; one txn = state + outbox/job; respect FDB/redb limits; no blobs in txns; workers do fan-out/delivery with idempotent retry.
- Update the relevant `docs/` contract in place; append one dated log to `state/logs/YYYY-MM-DD-<topic>.md`; never rewrite past logs.
- Gate: `cargo test --release` (last green 349/349 on `e24d75aea1085a06`), plus media-focused bench re-run (`features` media rows + S4) or a minimal live interop check vs real Mastodon 4.7.3. Paste or reference real output in the state log.

## Deliverables

- Per-issue verdict: OURS (fix) / MASTODON-SIDE / BENCH-ARTIFACT / HARNESS-TOO-STRICT, each with wire payload + file:line.
- Fixes (if any) + regression tests.
- Updated compat note if Mastodon requires a shape we now emit.

## Non-goals

- Do not touch polls, reactions, unboost/Undo, S6 churn tombstones, or pinetree code. Do not fix Mastodon, Misskey, Catodon, or Akkoma themselves.
