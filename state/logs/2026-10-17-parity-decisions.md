# 2026-10-17 — Parity decisions: 98% bar, search default, JSON bodies moved up, register review

Follows `state/logs/2026-10-17-compat-bar-domain-blocks.md`, which stays unchanged. Where this entry differs from it, this entry supersedes it. Documentation only. No code, tests or scripts changed in this session.

Phase reference: `birchtree.md` §1 (compatibility bar), §7 (Phase 4.1, Phase 7.1, Phase 7.2), §8 (item 11), §9, §10.

## Operator decisions

| Item | Decision | Where recorded |
|---|---|---|
| D1 `gifv` never produced | Intentional, accepted. | Deviation register (accepted) |
| D5 read-time ancestors | Intentional, accepted. | Deviation register (accepted) |
| D6 search default | **On by default** (`embedded`). An administrator can switch it off (`off`). Before Phase 7.3 the switch is a configuration value; from Phase 7.3 it is an admin setting. | `birchtree.md` §7 Phase 7.2; register D6 (accepted); manifest search note; profile; Feature-Matrix |
| Compatibility bar | Not 100%. **98% or better** of the parity suite's observations match upstream. The core flows (sign-in, compose, timelines, notifications, media) must match in full (my proposal). Every deviation the suite can see is still recorded in the register. | `birchtree.md` §1, §8 item 11; profile “Compatibility bar”; About; Roadmap |
| JSON request bodies (D11) | **Moved up** to a new **Phase 4.1**, before Phase 5. I recommend this: a client that sends JSON fails today, and streaming should not start on top of that gap. | `birchtree.md` §7 Phase 4.1 (new); register D11; Roadmap row 4.1; Feature-Matrix; Current-State; manifest notes |
| Exclusive lists on home (D10b) | Added to Phase 4.1. It is a small, verified fix. | `birchtree.md` §7 Phase 4.1; register D10b; manifest notes |

## Explanations (what the register now says)

### D3 — video target format (open)

- **Upstream** (`lib/paperclip/transcoder.rb`, v4.7.3): the target is MP4. WebM and QuickTime are converted to MP4 (`VIDEO_CONVERTIBLE_MIME_TYPES`). Files whose codecs are already eligible are passed through without re-encoding.
- **Plan**: video → WebM with VP9, decided on 2026-10-17 before this check.
- **Effect**: every video's bytes, `url` and `meta` change. Playback changes too: upstream's compatible videos are not re-encoded, and native mobile players have historically been weak on WebM, which is a client-visible failure.
- **Recommendation**: match upstream (MP4 with passthrough for compatible files). Keep VP9 WebM for animated GIF only, if you still want it there (D2). I have not verified current native-player support here; the parity suite is what will show it.

### D4 — still-image target format (open)

- **Upstream** (`app/models/media_attachment.rb`, v4.7.3): `file_styles` converts only `IMAGE_CONVERTIBLE_MIME_TYPES` (HEIC, HEIF, AVIF) to JPEG. Every other still image keeps its format (`IMAGE_STYLES`), with a `small` preview generated.
- **Plan**: all still images → one target format, format open.
- **Recommendation**: match upstream. Convert only HEIC, HEIF and AVIF to JPEG, and keep every other format. Stripping metadata, or any other re-encoding, would be an extra deviation and should be recorded as one.

### D9 — home `206` (recommended: accept)

- **Upstream** (`app/controllers/api/v1/timelines/home_controller.rb`): returns `206` with `Mastodon-Async-Refresh` (`retry=5`) while the home feed regenerates.
- **Birchtree**: computes the home feed when it is asked, so it never has a half-built feed and never needs `206`.
- **Effect**: only the status code and the header's timing differ. A first load is complete at once, where Mastodon's may be partial with a retry.
- **Recommendation**: accept. Matching it would mean inventing a regeneration state with no purpose.

### D10 — home timeline gaps

- **D10a, the 500-follow cap (recommended: accept, and measure in Phase 8).** Upstream builds the home feed from every follow and keeps the most recent posts overall. Birchtree reads only the 500 most recent follows. Only accounts that follow more than 500 accounts can miss posts. Removing the cap needs a bounded multi-author merge, which is a performance design for Phase 8, not a parity fix.
- **D10b, exclusive lists (gap, fix in Phase 4.1).** Upstream's `FeedManager#filter_from_home` hides members of exclusive lists from home (`:skip_home`). Birchtree stores `exclusive=true` but does not apply it, so members show on home where Mastodon hides them. Verified in the pinned tag.
- **D10c, tag-feed reblogs (closed).** The open question is settled. Upstream's `ReblogService` creates reblog rows with no tag rows (`statuses_tags`), so upstream's tag feed also excludes reblog wrappers. Birchtree already does the same. Pinned test `tag_timeline_normalizes_case_and_keeps_replies` unchanged.

## Other upstream facts verified in this session

- Upstream accepts JSON parameter bodies by default. No `parameter_parsers` override was found in `config/` (grep of `config/application.rb` and `config/initializers`).
- Upstream converts silent video to `gifv` as well as animated GIF (`lib/paperclip/transcoder.rb`, the audio-codec check). The register's D1 row now says so.

## Documents changed

- `birchtree.md`: the bar (98%, core flows in full); §8 item 11 scoring rule; §7 Phase 4.1 (new: JSON bodies, exclusive lists, tag-feed closed); Phase 7.1 (video and still-image decisions marked under review, open); Phase 7.2 (search on by default, admin switch); §9 open decisions and the decided list.
- `docs/compatibility-profile.md`: bar paragraph; register table rewritten (statuses D1–D13 and the recommendations); Phase 4 open items; Phase 2 JSON note; planned surface (Phase 4.1); open decisions (D3, D4, D6 closed).
- `docs/endpoint-manifest.md`: JSON-body note (Phase 4.1); exclusive lists (Phase 4.1); tag-feed note (closed); home-cap note (D10a); tag timeline row; search default note (D6 accepted).
- `docs/wiki/`: About (bar at 98%); Roadmap (98% bar; Phase 4.1 row); Feature-Matrix (timelines row; JSON bodies row; search row); Current-State (Phase 4.1 items; tag feed closed; home-cap note).

## Open items for the operator

1. **D3:** video target format: match upstream's MP4 (recommended), or keep VP9 WebM?
2. **D4:** still images: match upstream (convert HEIC, HEIF and AVIF only; recommended), or transcode all?
3. **D9:** accept (recommended)?
4. **D10a:** accept the 500-follow cap for now, and measure in Phase 8 (recommended)?
5. **D10b, D11:** confirm Phase 4.1 goes ahead before Phase 5.
6. **Search default:** confirm the admin switch can wait until Phase 7.2/7.3, or needs a configuration value sooner.

## Follow-ups outside this session's file scope

- `AGENTS.md` still says the current phase is Phase 5. If Phase 4.1 goes first, that line needs to change. I did not edit it, because it is a rules file.
- `docs/phase-4-exit-criteria.md` lists the exclusive-list and JSON gaps as Phase 4 open items. That file is off-limits here, so it still reads as it did at exit.
