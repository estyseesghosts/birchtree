# 2026-10-19: poll federation + bench run3

Implemented poll federation (D29 reversed, `docs/compatibility-profile.md`
D29/D34 and `docs/phase-6-exit-criteria.md` updated): `Question` wire type,
`Create`/`Update`-of-`Question` inbound, vote-shaped `Note` tallying,
`Question`/`Update(Question)`/vote-`Create` outbound, `NoteOrQuestion` fetch
type, `sync_remote_poll` + `poll_vote` federation events, `PollExpired`
redistribution. Vendored crate untouched.

Post-run forensics on run3 (1069/101 features) root-caused two more product
bugs, both fixed with regression tests and verified live: reblog-wrapper
URIs minted on the target's domain (Announce refused by strict receivers;
now booster's actor URI) and `closed`-as-time misread as ended (akkoma/
Sharkey always send `closed`; now compared against now). Full suite:
`cargo test --release` 340 passed / 0 failed, `cargo check` zero warnings.

Evidence: `tests/phase6_content.rs` (12 poll tests); bench report and logs
live in the pinetree repo (`docs/federation-bench-2026-10-19-phase7-run3.md`,
`...-run3-log.md`). Nothing committed, per the handoff rule. Open: S2/S6
classification, akkoma-outbound family, misskey undo, catodon tallying,
422/403 attribution (problems 45-50).
