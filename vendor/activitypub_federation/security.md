# Security follow-ups: axum vs actix-web inconsistencies

Fixes for the missing `verify_body_hash` call in the axum inbox are on local branches:

- `axum-body-verify` (based on `main`)
- `0.6-axum-body-verify` (based on the 0.6 branch)

Need to push and release them. Then publish new Ibis version with fix. Notify hatsu.

The two items below are still unfixed and need to be picked up.

## 1. Missing `signing_actor` helper for axum (~35 LOC)

`actix_web/mod.rs` exposes `signing_actor()` (digest check via `verify_body_hash` +
signature verification through `http_signatures::signing_actor`) for GET/object-fetch
flows. The axum module has no equivalent, so axum users must hand-roll signature
verification, which is error-prone (the missing digest check in `axum/inbox.rs` was
exactly this kind of mistake). Fix: add `signing_actor` to `src/axum/mod.rs`, copy logic from actix-web code.

## 2. Unbounded body read in axum inbox (~4 LOC)

`src/axum/inbox.rs` `ActivityData::from_request` uses
`axum::body::to_bytes(body, usize::MAX)`. Unlike actix (default 2 MB payload limit),
this buffers the request body into memory with no size cap *before* any digest or
signature check runs — a DoS vector. Fix: replace `usize::MAX` with a 2 MB limit