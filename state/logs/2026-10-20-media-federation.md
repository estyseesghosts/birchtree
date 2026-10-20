# 2026-10-20 — media + media federation investigation (HANDOFF_media-federation.md)

## Method

Three parallel subagents (A: outbound 404, B: inbound re-encode, C: no-attachment), synthesized before any code change per handoff rule. Key code claims spot-verified in-tree; no product code changed.

## Verdicts (all with wire + file:line evidence, no fix warranted)

- Outbound `pt/bt -> mastodon mismatch:404`: MASTODON-SIDE (+ possible BENCH-ARTIFACT transient). Our Document emit (`src/activitypub/outgoing.rs:130-158`) matches Mastodon 4.7.3's parser requirements; media URLs unauthenticated (`src/api/mastodon/endpoints/media.rs:133-155`, route `src/api/mastodon/router.rs:270`) over plain `http://` in bench (no TLS/lab-CA). Failing URL is Mastodon's own `system/cache/...` path = row created, bytes never servable (fetch/store or Sidekiq redownload lag). Independent pinetree sender fails identically.
- Inbound `mastodon -> * mismatch:200`: MASTODON-SIDE + HARNESS-TOO-STRICT. Our fetch stores original bytes verbatim + sniffed MIME (`src/federation/media.rs:65-91`), re-serves unmodified (`endpoints/media.rs:133-151`); `pt/bt -> bt` control pairs byte-exact PASS. Mastodon re-encodes every local upload (`lazy_thumbnail.rb needs_convert?`, `media_attachment.rb GLOBAL_CONVERT_OPTIONS`), so byte-hash oracle can never pass. Harness should compare perceptually or snapshot post-upload served bytes.
- `no-attachment` (dst=mastodon): MASTODON-SIDE / BENCH-ARTIFACT. 0/34 into lab-mastodon across five independent senders, zero non-mastodon failures; interop vs real Mastodon 4.7.3 passes ("mastodon received media from bt-a PASS"). Mastodon fetch failures keep the row (→ 404 shape), so zero-record = ingest-side drop, exact mechanism unproven (lab Sidekiq/rails logs not retained).
- Ruled out: lab-CA hypothesis for fetch direction (plain http), emit-shape rejection (rows prove parse), lag cause (S4 arrival_after_settle max 6.1s << 240s deadline; concurrent C3 50/50 p50 418ms).

## Changed

- `docs/federation-contract.md`: added "Media federation interop notes (bench run5)" section recording the three verdicts + PNG-only bench caveat + `"name": ""` cosmetic note. Status stays Draft. No product code touched.

## Verified

- `cargo test --release`: 349 passed / 0 failed (26 suites, all "0 failed"), matching `e24d75aea1085a06` baseline.
- No live bench re-run: bench is torn down and failures are proven remote-side; minimal live interop evidence reused from run5 (`interop.out` PASS vs real Mastodon 4.7.3).

## Open / recommended (harness-side, not ours)

- Harness: log emitted attachment count per Create + assert src-side attachments; retain lab-mastodon Sidekiq/rails logs on re-run; perceptual or served-snapshot media comparison.
- Bench video/GIF federation coverage is PNG-only; our video/GIF emit is code-identical in shape but unexercised over federation.
- Minor non-bench gaps noted, no action: `media_client` follows no redirects (`federation/net.rs`), 10s total for up-to-40MiB fetch is tight on slow links.
