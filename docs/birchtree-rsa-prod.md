# Birchtree production-hardening implementation guide

**Baseline audited:** `estyseesghosts/birchtree`, `master`, commit `8e85a55f25c3fdfe7eb26232f6b2f0d0e008ff1d` (21 October 2026).

**Purpose:** fix the verified correctness, security, storage, coupling, and operational risks before public production use. Keep the modular-monolith architecture and `redb`; do not turn this into a rewrite.

## Current state (authoritative checklist) — as of 2026-10-22

This section is authoritative. Where work packages below contradict it,
this section wins. Work package 4 (`§4.1`–`§4.3`) described the
pre-migration signer; that work landed in commit `7a8cd81`
(`WP4: replace RustCrypto RSA with AWS-LC-RS for federation signatures`,
2026-10-21) and is archived under `Archived (resolved)` inside Work
package 4. The Ed25519 + RFC 9421 dual-stack is explicitly remaining work
and a release gate (see `docs/ed25519-rfc9421-plan.md`).

**Shipped (aws-lc-rs):** the RSA provider migration (formerly §4.3, now
archived below) landed in `7a8cd81`.
The old `rsa` crate was removed from the tree (`Cargo.toml` documents the
removal; `cargo audit` is green on RUSTSEC-2023-0071).
`vendor/activitypub_federation/src/rsa_provider.rs` is the single AWS-LC
touchpoint: 2048-bit generation, PKCS#8 import, `sign_legacy_rsa_sha256`
(blinded inside the AWS-LC operation — no unblinded path exists) and
`verify_legacy_rsa_sha256` against SPKI PEM via the `spki` crate (no
hand-parsed DER). Stored PKCS#8 PEM keys still load, key IDs are stable,
and an OpenSSL-produced fixture signature verifies byte-identically
(`tests/a_rsa_provider.rs`). Outbound stays legacy draft-Cavage
`rsa-sha256`; inbound verifies legacy RSA and refuses unsigned requests
(401, no state change — `tests/phase6_federation.rs`).
Per-account single-flight key generation also landed in `7a8cd81`
(`src/activitypub/key_generation.rs`): the atomic store insert decides
the winner and discards losing generated keys.

**Compatibility exception (deliberate, not a migration gap):** legacy RSA
outbound is kept for RSA-only peers (older Mastodon, Misskey/Sharkey and
other implementations without RFC 9421 support). Dropping PKCS#1 v1.5
outbound unilaterally would break those peers, so the default stays
Cavage `rsa-sha256` until the dual-stack below ships.

**Explicitly remaining work (not done): Ed25519 + RFC 9421 dual-stack.**
No Ed25519 key is generated, published, or verified anywhere in the tree,
and no RFC 9421 `Signature-Input`/`Signature` path exists; an RFC
9421-shaped request is refused (401), never accepted and never crashes
(pinned by `rfc9421_shaped_request_is_refused_without_state_change`).
The design, when it ships, is dual-stack per peer — never
Ed25519-only-outbound (full plan in `docs/ed25519-rfc9421-plan.md`):

1. Generate and publish an Ed25519 key alongside the existing RSA key
   (no key rotation: the RSA `keyId` stays).
2. Outbound: a peer that advertises an Ed25519 key / accepts RFC 9421 gets
   RFC 9421 signatures only. A failed Ed25519 sign fails the delivery; it
   never silently falls back to RSA for that peer.
3. Outbound to RSA-only peers keeps `sign_legacy_rsa_sha256` under this
   compatibility exception.
4. Inbound verifies RFC 9421 and legacy RSA; unsigned stays refused.
5. Interop exit: one Mastodon 4.7-shaped RFC 9421 request accepted, one
   legacy draft-cavage request accepted, one bad signature rejected —
   proven by a live 4.7 lab run, not by unit fixtures alone.

### Release checklist (single authoritative list)

Do not call Birchtree production-ready until all of these are demonstrated
on the release build:

- [x] RSA provider migrated to AWS-LC-RS; legacy Cavage `rsa-sha256` wire
  format preserved; `cargo audit` green on RUSTSEC-2023-0071 (landed in
  `7a8cd81`; covered by `tests/a_rsa_provider.rs`).
- [x] Per-account single-flight actor key generation; atomic store insert
  decides the winner (`src/activitypub/key_generation.rs`, also `7a8cd81`).
- [ ] Ed25519 + RFC 9421 dual-stack per `docs/ed25519-rfc9421-plan.md`
  (release gate; until then
  `rfc9421_shaped_request_is_refused_without_state_change` must keep
  passing).
- [ ] Confirmation/reset bearers are non-predictable, protected at rest and
  removed/redacted after delivery; token consumption is atomic.
- [ ] Rate-limit client identity cannot be spoofed through untrusted
  forwarding headers.
- [ ] Existing blobs survive failed replacement; data/database/media
  directories have enforced permissions.
- [ ] File size and decoded-image limits are enforced; uploads and downloads
  are streamed; expensive processing is bounded and off the async executor.
- [ ] Durable and cache roots are truly separate; cache accounting restores
  after restart; eviction and reconciliation actually run.
- [ ] No staging files are publicly served; upload/metadata failures clean up
  or are reconciled.
- [x] Background jobs have bounded class-specific concurrency, retention,
  redaction and reliable crash recovery.
- [ ] Authorized-fetch and ordinary federation tests both pass for the target
  compatibility matrix.
- [ ] Restart/clock rollback cannot duplicate IDs; MFA lookup has an account
  index.
- [ ] Full CI, relevant smoke suites, Mastodon 4.7.3 compatibility tests, and
  backup/restore + realistic load tests pass.
- [ ] README, architecture map, current-state docs and TODOs describe the
  actual code and the remaining deviations accurately.

## Working rules

1. Work in small, independent commits. Do not combine crypto migration, storage lifecycle, and broad refactoring in one patch.
2. Add a regression test for each defect before or with its fix. Prefer tests through the public HTTP/runtime path as well as isolated unit tests.
3. Do not mark a feature “implemented” because a helper or test double exists. Prove that the production runtime invokes it and that the real `redb` + filesystem path behaves correctly.
4. Treat payloads, database files, backups, and logs as sensitive. Never log credentials, bearer tokens, private keys, TOTP seeds, or reset URLs.
5. Preserve protocol compatibility deliberately. Any reduction in federation compatibility must be an explicit product decision, demonstrated by an interoperability matrix, not an accidental side effect of changing a crypto primitive.

## Priority summary

| Priority | Work | Why it blocks or threatens production |
|---|---|---|
| **P0** | Replace predictable confirmation tokens; secure bearer storage | Account verification can rely on a guessable bearer; reset/confirmation secrets are retained in plaintext. |
| **P0** | Fix blob atomic replacement and private storage permissions | A failed rename can remove the previous blob; sensitive data can be readable under permissive existing directories. |
| **P0** | Fix rate-limit client-IP trust | `X-Forwarded-For` is accepted directly as the rate-limit identity and can be spoofed when the application is reachable. |
| **P0** | Harden the RSA signing operation without accidentally breaking federation — RESOLVED in `7a8cd81` (see Current state above and Archived in §4) | Was a vendored-signer side-channel hardening item; the provider migration has landed and `cargo audit` is green on RUSTSEC-2023-0071. |
| **P0** | Enforce `max_file_size` correctly and bound media resource usage | The upload handler substitutes the overall request-body limit for the configured file limit; uploads and responses buffer whole files. |
| **P1** | Finish cache-pool, eviction, and reconciliation runtime wiring | The cache store is opened and discarded; maintenance job arms are no-ops, and no production `ReconStore` implementation exists in the current source tree. |
| **P1** | Make email-token redemption and state mutation atomic | A consumed token can be lost if the corresponding account update fails. |
| **P1** | Bound and isolate background work; add retention | One serial worker handles unrelated jobs; completed job payloads, including sensitive data, persist. |
| **P1** | Close federation fetch/authorization gap and improve key handling | Remote-media dereferences are unsigned despite the federation contract; local signing-key creation has a costly race. |
| **P2** | Refactor oversized modules and narrow ports | Large modules and the aggregate `Store` interface weaken module boundaries and make review harder. |
| **P2** | Complete integration, abuse, recovery, and compatibility qualification | CI currently runs the whole Rust test suite but only the Phase 1 smoke script. |

---

# Work package 1 — Email tokens and secret handling (P0)

**Relevant files:**
- `src/jobs/handlers.rs`
- `src/application/accounts.rs`
- `src/api/mastodon/endpoints/accounts.rs`
- `src/api/oauth/confirmation.rs`
- `src/persistence/redb/accounts.rs`
- `src/persistence/redb/jobs.rs`
- `src/domain/user.rs`

## 1.1 Remove deterministic registration-confirmation tokens

The registration transaction queues `SendConfirmationEmail` without a token. The worker falls back to:

```rust
sha256_hex(format!("confirmation:{}", job.id).as_bytes())
```

`Id` is a timestamp-oriented identifier, not a secret. Hashing a predictable ID does not make a secure bearer token.

**Implement:**

- Generate a cryptographically random token before queuing the registration confirmation job.
- Persist only a token hash for validation, indexed by the hash; do not use the plaintext token as the `EMAIL_TOKENS` key.
- Ensure retryable mail delivery can recover the bearer without making it publicly readable in the job table. Prefer encrypted short-lived outbox payloads or a protected secret reference. If plaintext must exist temporarily in an active job, redact it atomically after successful delivery and never retain it in a completed job.
- Do not add a deterministic fallback. Missing token data must fail the job visibly and be retried/quarantined under the normal policy.

## 1.2 Protect password-reset and confirmation bearers at rest

`EMAIL_TOKENS` currently uses plaintext token strings as keys. Password-reset job payloads also include the plaintext token. Completed job records remain in the database with their payloads.

**Implement:**

- Store a one-way digest of each random bearer in `EMAIL_TOKENS`.
- Keep recoverable token material only in an encrypted, access-controlled outbox payload while delivery retries are possible.
- After successful send, remove/redact token and recipient-sensitive payload fields while preserving non-sensitive job diagnostics.
- Prune expired reset tokens, superseded tokens where policy permits, and completed-job payloads through bounded maintenance batches. Define retention explicitly; confirmation-token lifetime should remain a deliberate compatibility/policy choice rather than an accidental consequence of never-cleaned rows.
- Ensure reset endpoint responses remain indistinguishable for known and unknown addresses when mail is enabled.

## 1.3 Encrypt long-lived application secrets

The `TOTP_SECRETS` table stores serialized `TotpRecord` data, including the TOTP secret. `ACTOR_KEYS` persists the local RSA private-key PEM in serialized records. Both are sensitive if the database or backups leak.

**Implement:**

- Add authenticated encryption for TOTP seed material and local private signing keys, with a key supplied outside the database and backed up/rotated under an operator-managed process.
- Use a standard authenticated-encryption construction from a maintained crypto library; include versioning and key identifiers in the encrypted record so encryption keys can rotate.
- Fail closed if a required decryption key is missing or authentication fails. Do not silently reset MFA or generate a new actor key.
- Document recovery: losing the encryption key must not be mistaken for a recoverable database migration.

## 1.4 Make token consumption and state changes atomic

`confirm_email()` calls `redeem_email_token()` and then `set_email_verified()` as separate storage operations. A failure between them consumes the token while leaving the account unverified. Apply the same review to password-reset redemption and password updates.

**Implement:** add application/persistence operations that atomically validate token kind/lifetime, consume the token, and update the credential record in one `redb` write transaction. Keep the single-use property under concurrent requests.

**Acceptance tests:**

- Registration tokens are random and unpredictable; job IDs cannot reproduce them.
- A token is stored as a digest and cannot be recovered from `EMAIL_TOKENS`.
- A queued/retried email can be sent, but successful completion leaves no bearer in the job record.
- Database write failure cannot consume a token without applying the matching state change.
- Two concurrent redemptions yield at most one success.
- Expired password-reset tokens fail; unknown and used token responses do not disclose which case occurred.
- A missing encryption key or tampered ciphertext fails closed.

# Work package 2 — Filesystem and blob-store correctness (P0)

**Relevant files:** `src/media/blob_store.rs`, `src/media/fetch_stream.rs`, `src/media/reconciler.rs`, `src/persistence/redb/mod.rs`, `src/main.rs`, `src/api/mastodon/endpoints/media.rs`, `src/application/media.rs`.

## 2.1 Fix destructive rename fallback

`FsBlobStore::put_atomic()` removes `dest` after *any* initial rename error, then retries the rename. The initial failure can be unrelated to “destination already exists”; deleting the destination can destroy the old blob and still fail to write the replacement.

**Implement:** use platform-correct atomic replace semantics. Never remove an existing destination in response to an arbitrary rename error. If the platform cannot atomically replace a destination, implement a carefully tested platform-specific operation or return an error while preserving the old file.

Also sync the containing directory where supported if the documented durability guarantee requires the rename to survive power loss. Create temporary files with exclusive creation (`create_new`) and robust cleanup.

**Acceptance tests:** inject failures before write, during write, at sync, and at replace/rename. In every failed-replacement case, the previous destination must still contain its original bytes. Successful replacement must never expose a partial file.

## 2.2 Enforce private permissions rather than attempting best-effort permissions

`prepare_data_dir()` rejects world-writable directories but accepts existing directories such as `0755`; the blob store ignores errors from its `set_permissions(..., 0700)` call. The database contains credentials and key material, so this is too permissive.

**Implement:**

- On Unix, enforce the documented private permissions on the data root, blob roots and database file; fail startup if permissions cannot be established.
- Define whether group access is supported. If not, reject group/other access instead of checking only `0o002`.
- Ensure newly created database files have restrictive permissions despite the process umask.
- Do not follow attacker-controlled symlinks when resolving blob keys. Validate path components and use no-follow/directory-relative file operations where supported, or otherwise prove that all parent directories are private and cannot be replaced.

## 2.3 Prevent staging files from being served as media

`serve_file()` calls `blobs.get(&path)` without checking that the key belongs to a registered public media object. `resolve()` blocks traversal but accepts any relative path inside the root. Atomic write staging names live inside the blob tree.

**Implement:** distinguish persistent public media keys from temporary/staging namespaces. Never serve temporary files. Validate a requested path against the blob-key shape and, where compatible with existing URL semantics, against a committed media/profile/emoji reference. Preserve the existing intended public URL behaviour for uploaded Mastodon media; do not accidentally make private statuses' attached files inaccessible if upstream exposes those URLs.

## 2.4 Clean up files when metadata commits fail

Media uploads stage the original and optional thumbnail before creating the `redb` row/job. If a later `put` or metadata transaction fails, earlier staged bytes can remain orphaned. The same cross-filesystem/database partial-failure pattern exists in profile images and generated previews.

**Implement:** use a staging guard/transaction object that records every staged key and deletes uncommitted artifacts on ordinary error paths. Continue to use reconciliation for process crashes, which destructors cannot handle. Make committed references visible only after bytes are successfully staged.

**Acceptance tests:** fault injection at every step of original upload, thumbnail upload, metadata commit, preview write, delete, and transcode replacement. Verify no committed record points to a missing blob and that failures either clean up immediately or converge through the reconciler.

# Work package 3 — Rate limiting and client-IP trust (P0)

**Relevant file:** `src/api/mastodon/rate_limits.rs`; route middleware in `src/api/mastodon/router.rs`; trusted proxy config in `src/config.rs` and `src/main.rs`.

`client_key()` takes the first `X-Forwarded-For` value directly from the request. It does not verify that the TCP peer is a trusted proxy. A client able to reach the application directly can choose its own rate-limit key. The limiter also clears the entire key map once it grows beyond 100,000 entries, which is especially problematic when the key can be attacker-controlled.

**Implement:**

- Resolve client IP using the actual peer address plus the configured trusted-proxy list. Honor forwarding headers only if the peer is trusted; parse the proxy chain according to one documented rule.
- Make rate-limiter state bounded with expiry/eviction rather than clearing every user's counters at once.
- Add route-specific protections for password login, token issuance, registration, password-reset requests, confirmation resends and other sensitive/authenticated flows. The current general 1,500-request/5-minute fixed-window budget is not sufficient protection for every sensitive endpoint.
- For password reset, add controls keyed by normalized account/address as well as source IP, without revealing whether an account exists.
- Keep the per-process limitation explicit. If horizontal/multi-process deployment is later introduced, rate-limit state must be shared or the effective limit multiplied by replica count.

**Acceptance tests:** spoofed `X-Forwarded-For` cannot create arbitrary identities from an untrusted peer; the configured trusted proxy chain resolves correctly; flooding new identity strings cannot clear other users' windows; login/reset endpoints have independent thresholds and return the expected 429 behaviour.

# Work package 4 — Harden the RSA federation signer (P0) — RESOLVED in `7a8cd81`

**Relevant files:**
- `Cargo.toml`, `Cargo.lock`
- `vendor/activitypub_federation/src/http_signatures.rs`
- `vendor/activitypub_federation/src/rsa_provider.rs`
- `src/activitypub/actor.rs`
- `src/activitypub/key_generation.rs`
- `src/persistence/redb/federation.rs`
- `src/federation/net.rs`
- `src/activitypub/` and the federation interoperability tests

**Resolved:** the provider migration landed in commit `7a8cd81`
(`WP4: replace RustCrypto RSA with AWS-LC-RS for federation signatures`,
2026-10-21). What changed there:

- The direct and vendored `rsa` dependency was removed; `aws-lc-rs`
  (1.18.2 in `Cargo.toml`) is the RSA provider behind the narrow
  `sign_legacy_rsa_sha256` / `verify_legacy_rsa_sha256` adapter in
  `vendor/activitypub_federation/src/rsa_provider.rs`.
- 2048-bit generation, PKCS#8 import, and SPKI PEM verification go through
  AWS-LC with no hand-parsed DER; stored keys still load and key IDs are
  stable (`tests/a_rsa_provider.rs`, including the OpenSSL fixture check).
- Legacy Cavage `rsa-sha256` wire format was preserved; RSA-PSS was not
  introduced as a drop-in.
- Per-account single-flight key generation landed in the same commit
  (`src/activitypub/key_generation.rs`); the atomic store insert returns
  the winning stored key and discards losing generated keys.
- `cargo audit` is green on RUSTSEC-2023-0071.

Do not re-apply the pre-migration steps below. They are retained only as
history in the `Archived (resolved)` subsection.

## Archived (resolved) — pre-migration signer notes (superseded by `7a8cd81`)

All verbs below describe the tree *before* `7a8cd81` and are preserved here
only as history. The finding had been that the vendored signer had used an
unblinded private-key operation, and the old tree had contained an
unpatched advisory against that dependency. The interim blinding note and
the provider-migration plan had been written before the migration; both
were completed when `7a8cd81` removed the old dependency, added
`vendor/activitypub_federation/src/rsa_provider.rs` on AWS-LC, and added
`src/activitypub/key_generation.rs` for single-flight generation. The
pre-migration acceptance checks had been: blinded private-key operation
with no silent fallback to an unblinded call; legacy Cavage headers
accepted by Mastodon 4.4/4.5/4.6/4.7 and Misskey/Sharkey targets; stored
PKCS#8 keys loading with stable key IDs; key generation/import/signing/
verification and malformed-key coverage; dependency review showing the old
crate gone; and no key-generation storm on concurrent first fetches.

Historical references for this archived finding:

- RustSec advisory: https://rustsec.org/advisories/RUSTSEC-2023-0071.html
- Pre-migration API note (superseded): https://github.com/RustCrypto/RSA/blob/master/src/key.rs
- Pre-migration security note (superseded): https://github.com/RustCrypto/RSA
- AWS-LC-RS RSA provider docs: https://docs.rs/aws-lc-rs/latest/aws_lc_rs/signature/index.html

Compatibility references (still current, also listed in Reference links):

- Mastodon 4.7 release: https://blog.joinmastodon.org/2026/08/mastodon-4.7/
- Mastodon version/signature compatibility table: https://github.com/mastodon/mastodon/blob/main/FEDERATION.md
- RFC 9421 HTTP Message Signatures: https://www.rfc-editor.org/rfc/rfc9421.html

# Work package 5 — Upload limits and bounded media processing (P0)

**Relevant files:** `src/api/mastodon/endpoints/media.rs`, `src/application/media.rs`, `src/media/processing.rs`, `src/media/transcode.rs`, `src/media/fetch_stream.rs`, `src/media/blob_store.rs`.

## 5.1 Enforce the configured file limit

The upload endpoint currently does:

```rust
let max_file_size = state.max_body_size as u64;
```

This uses the entire HTTP body limit as the file limit, despite `ServerInfo` carrying `max_file_size` and configuration allowing `max_file_size < max_body_size`. A multipart request can therefore exceed the intended per-file limit when the body limit is larger.

**Fix:** pass the configured `state.server.max_file_size` (or an explicitly named validated field) into the application use case. Keep the total request-body limit separate so multipart overhead and optional metadata are accounted for without weakening the file limit.

## 5.2 Stream uploads and public file responses

`upload_fields()` collects each multipart field using `field.bytes()` into a `Vec<u8>`. `FsBlobStore::get()` reads whole files into a `Vec<u8>`, and `serve_file()` buffers the whole response. Large concurrent media requests can therefore multiply memory use.

**Implement:**

- Consume multipart files in bounded chunks into a temporary file, abort when the configured file cap is crossed, and remove partial files on failure.
- Stream file responses from disk with correct content length/type and cancellation cleanup; avoid reading the entire blob into memory just to serve it. Implement and test HTTP byte-range requests (`Range`, `206`, `Content-Range`, `Accept-Ranges`) for large audio/video files so seeking does not require downloading the whole file.
- Preserve MIME sniffing using a bounded prefix and validate the complete file with the actual decoder/probe.
- Add image dimension/pixel/allocation limits before or during decode. A compressed byte cap alone does not bound decoded image memory.
- Run image decode/resize and FFmpeg off Tokio executor threads. Use bounded blocking concurrency so uploads cannot exhaust the blocking pool. For the synchronous v1 API, preserve the response contract while offloading the work; for delayed v2 processing, commit the job and respond `202` where the format requires deferral.
- Retain existing no-shell process invocation, process wall-time/output/memory limits, and cleanup of process/temp files. Add explicit limits on concurrent FFmpeg processes.

**Acceptance tests:** configured file limit is enforced independently of body limit; an oversized field is aborted before the full payload is resident; malformed multipart cleans temporary files; decompression-bomb-like images are rejected within memory limits; cancellations/disconnects clean staging files; concurrent video/image uploads stay inside a measured memory/CPU budget.

# Work package 6 — Complete the cache and reconciliation lifecycle (P1)

**Relevant files:** `src/main.rs`, `src/application/ports.rs`, `src/persistence/redb/remote_cache.rs`, `src/media/remote_cache.rs`, `src/federation/media.rs`, `src/media/reconciler.rs`, `src/jobs/handlers.rs`, `src/jobs/worker.rs`, `src/config.rs`.

The server opens durable and cache roots, then discards `cache_blobs` (`let _ = cache_blobs`) and passes the durable root into the main application/federation context. Remote media is still read/written through that handle. In addition, `CacheEvict | ReconcileStorage` are no-op arms in the generic job handler. The reconciliation helper exists, but that does not mean production invokes it.

**Implement:**

1. Make `BlobStores` the runtime dependency and route local uploads/profile media to the durable pool and remote cache bytes to the cache pool. Keep the database metadata and blob-pool choice consistent.
2. Rehydrate the in-memory cache ledger from `REMOTE_CACHE`/`CACHE_ACCOUNTING` during startup, or rebuild it through a bounded, resumable scan before accepting cache traffic. An empty in-memory ledger must not mean “zero bytes stored” after restart.
3. Implement `ReconStore` for `RedbStore` (the current tree only has the `MemoryStore` test implementation), then implement real `CacheEvict` and `ReconcileStorage` handlers and schedule them. Make every batch bounded and resumable, using existing cursor/pagination contracts. Tests against the in-memory fake alone do not qualify.
4. Ensure eviction order is safe across the database and filesystem: remove/update the mapping and accounting transactionally, then delete only that row's blob key; reconcile crash leftovers. Never evict local durable media.
5. Choose one documented fetch policy. If remote media is on-demand, stop unconditional eager fetch enqueueing and enqueue on a cache miss, with a deduplicated single-flight job. If optional scoped warming is enabled, keep it distinct and capped.
6. On restart, test missing cache bytes, orphaned cache bytes, stale metadata, interrupted delete, corrupted ledger data, and a full disk.
7. Validate resolved/canonical durable and cache paths, not only string/path equality. Refuse identical roots and unsafe overlapping roots, including symlink aliases.

**Acceptance tests:** after restart, the cache accounting equals actual stored bytes; concurrent reservations never exceed the cap; eviction deletes only cache-pool files; durable local uploads survive; a missing cache blob re-fetches; reconciliation converges to zero discrepancies on repeated runs; configured path aliases/overlaps are refused.

# Work package 7 — Job queue reliability, isolation, and retention (P1)

**Relevant files:** `src/jobs/worker.rs`, `src/jobs/handlers.rs`, `src/persistence/redb/jobs.rs`, `src/domain/job.rs`, `src/main.rs`.

The runtime uses one worker loop. It claims a batch, then awaits each network job and runs synchronous handlers serially. A slow origin or transcode can delay unrelated notifications, confirmation mail, federation delivery and maintenance work. Completed jobs retain their payloads. This is not an argument for unbounded concurrency.

**Implement:**

- Keep durable leases/retries/quarantine, but use bounded pools or semaphores for distinct workload classes: local jobs, federation/network jobs, and CPU/media jobs.
- Limit concurrent work per remote host to protect both this instance and remote peers.
- Use the idempotency keys already designed for jobs/side effects; ensure retry after “side effect succeeded, completion write failed” does not duplicate a notification, mail or delivered activity where the downstream operation can be made idempotent.
- Implement retention for succeeded/quarantined job records and truncate sensitive payloads after success. Preserve job IDs, kind, timestamps, attempts, outcome and a bounded/redacted error record.
- Ensure shutdown stops claiming new work, gives active work a bounded drain period, and leaves recoverable leases if the deadline expires.
- Do not treat `CacheEvict` and `ReconcileStorage` as successful no-ops. Tests must fail if a maintenance job is dispatched without a real implementation.

**Acceptance tests:** a slow remote host does not starve local jobs; concurrency remains under configured limits; retries are idempotent; crashes recover leases; completed payloads no longer contain bearers; a permanently failing job is quarantined with a useful, redacted diagnostic.

# Work package 8 — Federated fetches and runtime security configuration (P1)

**Relevant files:** `src/federation/media.rs`, `src/federation/net.rs`, `src/config.rs`, `src/main.rs`, `src/api/mastodon/router.rs`.

## 8.1 Signed remote-media dereferencing

`src/federation/media.rs` explicitly documents that the remote attachment `GET` is unsigned while the federation contract calls for instance-actor-signed dereferences. This can fail against secure/authorized-fetch instances and leaves the contract incomplete.

**Implement:** use the same explicit remote-fetch policy and signing subsystem as other ActivityPub dereferences. Add fixtures for both ordinary public objects and authorized-fetch peers. Keep URL validation, DNS/private-IP protections, redirect revalidation, bounded response size and timeouts for every redirect hop.

## 8.2 Insecure runtime knobs must not be production footguns

The runtime warns when lab federation switches are active or `rate_limit_multiplier > 1`, but continues to start. The multiplier increases the allowed rate-limit count and is explicitly described as weakening throttles.

**Implement:** introduce a deliberate development/lab mode, separate from production configuration. Refuse to start with private-network/HTTP federation allowances or weakened rate limits in production mode unless an unmistakable explicit override is supplied. Emit structured startup configuration summaries without secrets. Never log SMTP credentials or VAPID private material.

## 8.3 Correct status and privacy behaviour for public media

Test the public media route with guessed keys, path traversal, backslashes, symlink paths, temp filenames and missing metadata. Ensure only intended media key namespaces are exposed and content is never served from partial staging files.

**Acceptance tests:** secure-fetch peers work; private IP/loopback destinations and DNS-rebinding attempts remain blocked by default; redirects cannot bypass URL policy; production mode refuses dangerous flags; temporary/staging keys return 404.

# Work package 9 — Identifier safety after restart (P1/P2)

**Relevant file:** `src/domain/id.rs`; startup wiring in `src/main.rs`; inserts in `src/persistence/redb/`.

`TimestampIdGenerator` keeps the last generated ID only in a process-local `Mutex`, initialized to zero. A restart in the same millisecond, clock rollback, or restoring an older database can reset the generator behind the highest persisted ID. Since generated IDs are used as primary keys, a collision can overwrite or conflict with existing rows.

**Implement:** initialize the generator above the highest persisted ID at startup, or persist/transactionally reserve a monotonic sequence. Guard primary-record inserts against unexpected duplicate IDs so a collision cannot silently replace an existing row. Define restore/clock-rollback behaviour.

**Acceptance tests:** restart within the same timestamp millisecond; simulated backward clock jump; restored database with a higher ID than wall clock; concurrent creation. IDs must remain unique and monotonic under the documented single-node model.

# Work package 10 — Application boundaries and god-files (P2)

**Relevant files:**
- `src/application/ports.rs` (~2,552 lines)
- `src/api/mastodon/endpoints/admin_infra.rs` (~1,968 lines)
- `src/media/transcode.rs` (~1,739 lines)
- `src/api/mastodon/endpoints/accounts.rs` (~1,662 lines)
- `src/main.rs` (~899 lines)
- `src/persistence/redb/statuses.rs` (large resource adapter)

**Implement incrementally:**

1. Split `application/ports.rs` declarations into resource-level modules, keeping existing traits and behaviour stable during the move.
2. Stop making every use case depend on the aggregate `Store` where possible. Inject narrow interfaces (e.g. account lookup, job enqueue, blob store, streaming sink) into the use case that needs them. Keep aggregate composition in startup/context code.
3. Split `admin_infra.rs` by bounded resource families (domain/email/IP blocks, settings/site content, announcements/metrics as appropriate). Keep shared validation/auth policy centralized rather than duplicating it.
4. Split transcode planning, process invocation/resource limits, and media metadata/commit logic into focused modules with explicit boundaries.
5. Extract CLI argument handling, runtime builders, and worker construction from `main.rs` without creating a generic dependency-injection framework.
6. Preserve the domain/application/API/persistence direction. Domain code must not import Axum, `redb`, filesystem adapters or ActivityPub DTOs.

**Acceptance tests:** no endpoint or use case changes behaviour during mechanical splits; `cargo check --all-targets --locked`, the full test suite and all smoke/interoperability gates pass after each step. Do not refactor huge modules and change protocol semantics in the same commit.

# Work package 11 — WebAuthn lookup and operational readiness (P2)

**Relevant file:** `src/persistence/redb/second_factor.rs`, `src/api/mastodon/router.rs`, `src/persistence/redb/mod.rs`.

`webauthn_for_account()` scans the entire `WEBAUTHN_CREDS` table to collect credentials for one account and stops at 64. This becomes O(total credentials) on the MFA login path and silently omits credentials beyond the cap.

**Implement:** add a `WEBAUTHN_BY_ACCOUNT` index maintained atomically with credential insertion/deletion; define and enforce a documented per-account credential limit rather than relying on a scan cap. Add a migration and conformance tests.

The `/ready` endpoint checks that storage can begin a read transaction, not that disk writes, worker progress or filesystem pools are healthy. Keep `/health` as liveness; enhance readiness/metrics without leaking sensitive details. Expose bounded metrics for queue backlog/age, retries/quarantine, disk usage, cache accounting/reconciliation status, media-processing latency, and worker health.

# Work package 12 — CI and release qualification (P0/P1)

**Relevant file:** `.github/workflows/ci.yml`, `scripts/`, `tests/`, `docs/TODOs.md`.

The latest pipeline runs `cargo check --all-targets --locked`, `cargo test --locked`, `cargo build --release --locked`, and only `scripts/smoke-phase1.sh`. The dependency-audit job fails; its log also reports a GitHub check-reporting permission error. Broad phase smoke scripts and live federation interoperability should not be manual-only gates.

**Implement:**

- Fix every actionable RustSec finding. Do not ignore an advisory merely to make the pipeline green; any accepted exception needs a documented reachability assessment, owner and expiry/review date.
- Fix GitHub Actions permissions for reporting checks and pin actions according to the project's supply-chain policy.
- Run the relevant smoke suites for API/OAuth, persistence/crash recovery, media/storage, streaming, federation, and admin/MFA in CI. Keep slow external/live-peer tests in a scheduled or pre-release workflow if they cannot run on every push.
- Add at least one actual Mastodon 4.7.3 client/API/federation compatibility job to the qualification gate. Record expected deviations explicitly.
- Add CI checks for schema-version documentation drift, stale TODO/status claims, unsafe staging-path serving, and unimplemented production job arms. The current `schema::SCHEMA_VERSION` is 21; synchronize the schema-conformance docs with the source. The README, current-state wiki and architecture tree have also contained contradictory/stale feature and layout claims—make one compatibility/deviation inventory authoritative.
- Define a release checklist with backup/restore, disk-full, restart, clock rollback, cache eviction, and realistic load tests.

## Production qualification gate

Superseded by the single authoritative list in `Current state
(authoritative checklist)` at the top of this file. That section is the
only checklist; this section is retained as a pointer so older links do
not resolve to a stale duplicate.

## Reference links

- Birchtree CI: https://github.com/estyseesghosts/birchtree/actions
- Current repository tree: https://github.com/estyseesghosts/birchtree/tree/master/src
- RustSec RSA/Marvin advisory (historical context for the resolved item above): https://rustsec.org/advisories/RUSTSEC-2023-0071.html
- AWS-LC-RS RSA provider docs: https://docs.rs/aws-lc-rs/latest/aws_lc_rs/signature/index.html
- RustSec `blurhash` advisory: https://rustsec.org/advisories/RUSTSEC-2023-0083.html
- RustSec `rustls-webpki` advisory: https://rustsec.org/advisories/RUSTSEC-2026-0382.html
- RustSec `webpki-roots` advisory: https://rustsec.org/advisories/RUSTSEC-2026-0381.html
- Mastodon 4.7 crypto/federation changes: https://blog.joinmastodon.org/2026/08/mastodon-4.7/
- Mastodon federation compatibility table: https://github.com/mastodon/mastodon/blob/main/FEDERATION.md
- RFC 9421 HTTP Message Signatures: https://www.rfc-editor.org/rfc/rfc9421.html
