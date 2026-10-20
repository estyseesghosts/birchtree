# vendor/

`activitypub_federation` 0.6.6, unmodified except for one patch (`[patch.crates-io]` in the root `Cargo.toml`):

- `src/reqwest_shim.rs`: `MAX_INBOX_BODY_SIZE` = 1 MiB, the inbox limit. Upstream 0.6.6 uses 200 KB for inbox bodies (`src/axum/inbox.rs`). Mastodon accepts 1 MB, so birchtree does too (owner decision, 2026-10-18; deviation D27 closed).
- `src/axum/inbox.rs`: the inbox extractor reads up to `MAX_INBOX_BODY_SIZE`.
- `src/http_signatures.rs`: `verify_signature` is `pub` (was `pub(crate)`), so birchtree can check the signature of a GET under authorized fetch. It is the crate's own check, unchanged.
- `src/http_signatures.rs`: the public key is trimmed and given a single trailing newline before `from_public_key_pem`, because Akkoma publishes `publicKeyPem` ending in a blank line (`\n\n`) that the strict parser rejects, which turned every Akkoma-signed activity into a 500. A key that still does not parse now fails the signature check (401) instead of a 500.
- `src/axum/inbox.rs`: when the signature fails against the cached key, the actor is fetched once more (`dereference_forced`) and the signature checked again, as Mastodon does. A copy fetched in the last 60 s is not refetched, so forged signatures cannot make the server fetch on every request. Without this, a key cached before the sender regenerated it kept failing (401) until the 24 h refresh interval. Covered by `tests/phase6_federation.rs`.
- `src/http_signatures.rs`: `#[allow(dead_code)]` on `signing_actor`, which only the actix-web feature uses. Registry builds cap lints for dependencies; a path dependency does not.

Why a patch and not an extractor of our own: `ActivityData` has no public constructor, and the signature and digest checks it feeds (`verify_signature`, `verify_body_hash`) are `pub(crate)`. A replacement extractor could not reach them without copying the protocol.

Remote document fetches keep the 200 KB limit. Licence: AGPL-3.0, as the crate.
