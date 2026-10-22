# 2026-10-22 — S1 ports split (decomposition proposal)

## What changed
S1 per `birchtree-decomposition-proposal.md`: `src/application/ports.rs`
(2673 lines) split into 26 submodules under `src/application/ports/`;
`ports.rs` is now a 57-line re-export shim (`pub mod` + `pub use self::*`).
Mechanical move only: zero behavior change, no signature changes, no S0-type
adoption.

New files: `account_store`, `oauth_store`, `session_store`,
`status_store`, `relationship_store`, `suggestion_store`, `web_store`,
`admin_infra_store`, `trend_store`, `annual_report_store`, `poll_store`,
`quote_store`, `notification_store`, `media_store`, `place_store`,
`list_store`, `marker_store`, `filter_store`, `scheduled_store`,
`emoji_store`, `search_store`, `admin_store`, `timeline_store`,
`interaction_log`, `collection_store`, `store` (supertrait + `StreamingSink`
+ `NoStreaming` + `ContextDeps` + `Ports` + `Clock`/`IdGenerator` re-export).
Largest new file 279 lines (`admin_infra_store.rs`); all under the 400-line
ceiling.

Delegation: 5 subagents (groups A–E), disjoint trait sets, new-files-only;
parent owned the cutover (`ports.rs` shim, `mod` wiring, cross-module
imports). Parent fixes at merge:
- `web_store.rs`: added `use super::suggestion_store::{FeaturedTag,
  TagFollow, WebPushSubscription};` (group C left bare sibling refs).
- `store.rs`: added `super::` imports for all 28 sibling traits (group E
  left them bare).
- `media_store.rs`: recovered the `MediaStore` trait (original lines
  1809–1855) which group D's spans missed (DTOs moved, trait dropped);
  verified via item-name diff of HEAD `ports.rs` vs new files: zero missing,
  zero duplicates after the fix.

Deliberately NOT moved (per proposal): `federation_ports.rs`,
`accounts.rs:SecondFactorStore`, `announcements.rs:AnnouncementStore`,
`conversations.rs:ConversationStore`, `push.rs:PushSubscriptionStore`.

## Verification (native Windows toolchain, no WSL)
- `cargo check --all-targets`: exit 0. Zero warnings from `ports/` files;
  remaining warnings pre-existing (`transcode.rs:pid`, `fix_*.rs`,
  `adv2_*` test imports).
- `cargo test` (full): 1133 passed, 0 failed, exit 0.
- `rustfmt --edition 2024 --check` on parent-touched files
  (`ports.rs`, `store.rs`, `web_store.rs`, `media_store.rs`): clean.
  Whole-tree `cargo fmt` NOT run (tree is not fmt-clean under rustfmt 1.99).
- `scripts/smoke-all.sh` NOT run: it requires `bash` (only WSL shims on
  this host) and the standing instruction mandates native Windows toolchain;
  its `cargo check` + `cargo test` substance is covered above.
- Item inventory: every `pub trait/struct/enum` in HEAD `ports.rs` exists
  exactly once across the new files.

## Open questions / next
- S2 (router split) and S3 (main/cli/bootstrap) are independent of S1 and
  unblock conflict-reduction; suggested next per proposal order S2 → S3 → S9.
