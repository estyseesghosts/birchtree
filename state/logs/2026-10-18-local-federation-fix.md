# 2026-10-18 — Local federation fix and debug rate limits (plan Phase 2)

Source plan: `/root/PLAN_LOCAL_FEDERATION_FIX_AND_REBENCH.md` §2.1, §2.2 (2.3 skipped: mastodon proxy is a later phase).

## What changed

**Federation (2.1), birchtree-side**
- `src/activitypub/activities.rs` `accept_for_follow`: the Accept's ID now lives on the accepting actor's URL (`<actor>#accepts/<follow id>`), not on the follower's domain. The crate requires an activity's ID and actor to share a domain, so the old `<follow id>#accept` Accept was refused by every receiver. Still deterministic per Follow.
- `src/main.rs`, `src/config.rs`, `src/federation/*`: no change. The `--federation-allow-http` and `--federation-allow-private` wiring is correct in the working tree. It is NOT in `HEAD`'s `main.rs`, which is why older builds showed `allow_http=false`.

**Federation (2.1), vendored `activitypub_federation` 0.6.6 (`vendor/activitypub_federation/src`)**
- `config.rs` `verify_url_valid`: an IP-literal host no longer fails with "Url must have a domain". It goes to birchtree's `UrlVerifier` (`PolicyVerifier`), which allows it only if public unicast or inside `--federation-allow-private`. The crate's private-IP DNS checks still apply to domains only.
- `activity_sending.rs` `generate_request_headers`: Host header uses `host_str()` (was `domain().expect(..)`, which panicked on IP-literal inboxes).
- `protocol/verification.rs` `verify_domains_match` and `fetch/mod.rs` object-id/redirect comparison: compare `host_str()` (was `domain()`, where two different IP literals both yield `None` and compared equal).

**Rate limits (2.2)**
- `src/config.rs`: `rate_limit_multiplier: u32` (default 1).
- `src/main.rs`: `--debug-weak-rate-limits` (=10), `--rate-limit-multiplier=N` (N >= 1; 0 or invalid exits 2). Warns at startup when > 1.
- `src/api/mastodon/mod.rs`: `ApiState.rate_limit_multiplier`. `router.rs` applies the middleware with state.
- `src/api/mastodon/rate_limits.rs`: limit = `DEFAULT_LIMIT.saturating_mul(multiplier)`.
- Tests constructing `ApiState` (`tests/common/mod.rs`, `tests/phase5_streaming.rs`, `tests/runtime.rs`) set `rate_limit_multiplier: 1`.

## Root causes found

1. `--federation-allow-http` / `--federation-allow-private` were not in committed `HEAD`; they exist only in the working tree. Older binaries reported `allow_http=false`.
2. With the flags in place, the "Localhost is only allowed in debug mode" check does not fire: `debug = !allowlist.is_empty()`, so the crate's production-only checks are off. That message appears only when the allowlist is empty.
3. Real blocker for `127.0.0.1` public URLs: the crate rejected IP-literal URLs before the policy ran ("Url must have a domain"), then panicked on IP-literal inboxes, then compared IP origins as equal, then birchtree generated an Accept on the wrong domain.

Security note: IP-literal URLs are not blanket-allowed. Without the allowlist, loopback and private literals are refused by `policy::check_url` (existing test `url_shape_refuses_credentials_and_ip_literals`). Public IP literals are now accepted in production, as public domains already are; the address policy is the same.

## Verification (executed)

- `cargo build`: finished, 0 warnings.
- `cargo test --all-targets`: exit 0, all suites `ok`, 0 failed, 0 warnings. One pre-existing `#[ignore]` measurement test (`phase4_1_home_cap.rs:192`).
- Federation, two nodes: `bt-a` (`--public-url=http://bt-a.lab:8080`, port 8080) and `bt-b` (`--public-url=http://127.0.0.1:8081`, port 8081), both with `--federation-allow-private=127.0.0.0/8 --federation-allow-http`.
  - bob (bt-b) resolves alice@bt-a.lab:8080; follow applied on bt-a; Accept applied on bt-b.
  - alice posts on bt-a; the post appears in bob's home timeline on bt-b.
- Rate limits: 1600 requests at concurrency 50 to `/api/v1/instance`.
  - `--debug-weak-rate-limits`: 1600 × 200, `x-ratelimit-limit: 15000`.
  - control (default): 1499 × 200, 101 × 429, `x-ratelimit-limit: 1500`.

## Open questions

- The vendored crate is patched in place (no upstream patch). Those edits are not in `HEAD` either; whoever commits must include `vendor/` changes.
- No dedicated unit test for the crate-level IP-literal path; coverage is the federation end-to-end run above.
- The plan's 50-request check cannot reach 429 at default limits; the 1600-request check is the substitute.
