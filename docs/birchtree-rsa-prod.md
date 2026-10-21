# Birchtree production-hardening implementation guide

**Baseline audited:** `estyseesghosts/birchtree`, `master`, commit `8e85a55f25c3fdfe7eb26232f6b2f0d0e008ff1d` (21 October 2026).

**Purpose:** fix the verified correctness, security, storage, coupling, and operational risks before public production use. Keep the modular-monolith architecture and `redb`; do not turn this into a rewrite.

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
| **P0** | Harden the RSA signing operation without accidentally breaking federation | The vendored signer calls RustCrypto `RsaPrivateKey::sign()` without blinding; the crate remains under an unpatched RustSec timing-side-channel advisory. |
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

# Work package 4 — Harden the RSA federation signer (P0)

**Relevant files:**
- `Cargo.toml`, `Cargo.lock`
- `vendor/activitypub_federation/src/http_signatures.rs`
- `src/activitypub/actor.rs`
- `src/persistence/redb/federation.rs`
- `src/federation/net.rs`
- `src/activitypub/` and the federation interoperability tests

## 4.1 What is wrong in the current code

The vendored `sign_request()` uses `RsaPrivateKey::sign(Pkcs1v15Sign::new::<Sha256>(), digest)` without supplying an RNG. RustCrypto's RSA API documents that `sign_with_rng()` uses the supplied RNG to blind the private-key operation for PKCS#1 v1.5 signatures. The current call therefore misses this side-channel mitigation.

Be precise about the finding: **PKCS#1 v1.5 here is a signature encoding, not an RSA decryption operation.** The classic Marvin padding-oracle scenario is associated with timing leakage from RSA private-key decryption and padding processing. The inspected signer does not itself decrypt remote ciphertext, so source inspection alone does not establish that Birchtree exposes that classic decryption oracle. However, it does perform network-facing private-key signing without blinding, and RustSec continues to flag the RustCrypto `rsa` crate for network-observable private-key timing leakage with no patched release. Treat the current operation as a P0 issue; do not claim the API substitution alone clears the advisory.

References:
- RustSec advisory: https://rustsec.org/advisories/RUSTSEC-2023-0071.html
- RustCrypto RSA private-key API and `sign_with_rng()` blinding semantics: https://github.com/RustCrypto/RSA/blob/master/src/key.rs
- RustCrypto RSA security warning: https://github.com/RustCrypto/RSA

## 4.2 Immediate mitigation: blind the existing signature operation (only if provider migration is not immediate)

This is a short-lived mitigation, not the preferred end state. If the implementer can complete the provider migration below in the same focused change, skip this interim dependency-bound patch and move directly to AWS-LC-RS. Otherwise, implement it as a separately reviewable stopgap.

**Implement in a small, isolated commit:**

1. Replace the current `RsaPrivateKey::sign(...)` call with `sign_with_rng(...)`, using a cryptographically secure RNG that implements the exact `rand_core` traits expected by the resolved `rsa` 0.9 dependency. Do not assume the application's direct `rand` version is trait-compatible; verify the actual dependency versions and use an appropriate CSPRNG.
2. Keep `Pkcs1v15Sign::new::<Sha256>()` for this immediate mitigation. Randomized blinding changes how the private operation is performed, not the required legacy signature format. This preserves the existing Cavage-style `rsa-sha256` wire protocol and should not require actors to rotate keys.
3. Put the operation behind a narrow helper such as `sign_legacy_rsa_sha256()` so the implementation can be replaced behind one interface later. Do not add a fallback to the unblinded `sign()` method if RNG initialization or signing fails; fail the delivery and report a sanitized error.
4. Add a regression test proving generated signatures verify under the existing RSA public key, and integration coverage showing the request still uses the expected legacy headers, signed components, digest, and RSA-SHA256 signature format.
5. Label this commit as a **mitigation, not final advisory resolution**. `cargo audit` will still report the unpatched `rsa` crate while it remains in the dependency tree.

## 4.3 Production fix: replace RustCrypto RSA with AWS-LC-RS

**Use `aws-lc-rs` as the selected replacement. Do not leave the provider choice open to the implementer.** AWS-LC-RS is a maintained Rust interface to AWS-LC's cryptographic implementation and supports RSA key generation, RSA PKCS#1 v1.5/SHA-256 signing, and RSA signature verification. Its documented API provides the functions needed to retain Birchtree's existing legacy federation signature format. Pin the resolved version in `Cargo.lock` and keep it current with security releases.

References:
- Crate and API: https://docs.rs/aws-lc-rs/latest/aws_lc_rs/
- RSA key generation, PKCS#8 import, and serialization: https://docs.rs/aws-lc-rs/latest/aws_lc_rs/rsa/struct.KeyPair.html
- RSA PKCS#1 v1.5 signing/verification APIs and required public-key encoding: https://docs.rs/aws-lc-rs/latest/aws_lc_rs/signature/index.html
- AWS-LC-RS source and build notes: https://github.com/aws/aws-lc-rs

**Implement:**

1. In `Cargo.toml`, replace the direct `rsa = { version = "0.9", ... }` dependency with `aws-lc-rs` (the current documented release at the time this guide was revised is 1.18.2; resolve and lock the current patched release rather than blindly freezing that version forever). Remove the old crate from the resolved production dependency tree after all direct and vendored usages have been migrated. Do not assume changing the top-level dependency is sufficient: `vendor/activitypub_federation/Cargo.toml` and its source currently use RustCrypto's `rsa` APIs and must be updated too.
2. In `vendor/activitypub_federation/src/http_signatures.rs`, use `aws_lc_rs::rsa::KeyPair` for RSA generation and private-key import, and `aws_lc_rs::signature::RSA_PKCS1_SHA256` for signing the **full signing string**. AWS-LC-RS's `KeyPair::sign` hashes the message as part of the operation; do not pass the precomputed SHA-256 digest to that message-signing API, or the implementation will hash a digest again and produce incompatible signatures. Size the signature buffer to `key_pair.public_modulus_len()`.
3. Generate 2048-bit actor RSA keys through `aws_lc_rs::rsa::KeyPair::generate(KeySize::Rsa2048)`. Serialize private keys to PKCS#8 DER via `AsDer<Pkcs8V1Der>` and retain the existing `-----BEGIN PRIVATE KEY-----` PEM representation in storage. On import, decode PEM to DER and load with `KeyPair::from_pkcs8`. Preserve the existing public-key `-----BEGIN PUBLIC KEY-----` SubjectPublicKeyInfo representation in actor documents. Use the existing ASN.1/SPKI tooling or AWS-LC's key parser for the public-key encoding conversion required by signature verification; **do not hand-parse ASN.1/DER**.
4. Implement legacy inbound RSA verification using `aws_lc_rs::signature::RSA_PKCS1_2048_8192_SHA256`. Its documented signature-verification API expects an ASN.1 `RSAPublicKey` encoding, while Birchtree's ActivityPub actor documents store an X.509 `SubjectPublicKeyInfo` PEM (`BEGIN PUBLIC KEY`). Convert this with an existing, maintained ASN.1/SPKI parser or a supported AWS-LC key conversion. Add tests for real actor-document PEM inputs, rather than testing only with generated raw DER fixtures.
5. Keep this change behind a small provider adapter. Do not expose AWS-LC types through domain/application traits. Preserve existing actor IDs, `keyId` values, stored private keys, and published public keys; changing the crypto implementation must not trigger key rotation. If an existing private key cannot be imported, fail safely and provide an explicit, tested migration path rather than silently generating a replacement identity.
6. Preserve the existing PKCS#1 v1.5 / SHA-256 wire format for legacy HTTP Signatures. **Do not replace it with RSA-PSS as a drop-in fix.** Many receivers expect the old Cavage-style `rsa-sha256` signature format; Mastodon's RFC 9421 support uses `rsa-v1_5-sha256` for RSA compatibility, and not every older peer supports RFC 9421 at all.
7. Check build and deployment implications of AWS-LC-RS's native cryptographic backend on every supported platform and in CI. Add the dependency's required native build tools to the documented build environment if needed. Keep the crate updated; choosing a different provider is not a substitute for dependency auditing.
8. Keep Ed25519 + RFC 9421 as a separate federation-capability enhancement, not a prerequisite for the RSA security fix. Mastodon 4.7 supports Ed25519 through FEP-521a/RFC 9421, but older Mastodon releases and other Fediverse implementations have different support. If adding Ed25519, make per-peer behavior explicit. Do not silently downgrade a failed Ed25519 attempt to legacy RSA unless that fallback is an intentional, documented policy.

Compatibility references:
- Mastodon 4.7 release: https://blog.joinmastodon.org/2026/08/mastodon-4.7/
- Mastodon version/signature compatibility table: https://github.com/mastodon/mastodon/blob/main/FEDERATION.md
- RFC 9421 HTTP Message Signatures: https://www.rfc-editor.org/rfc/rfc9421.html

## 4.4 Keep RSA key-generation races separate

`keys_for_local()` checks for an actor key, generates a 2048-bit keypair on a blocking thread if missing, then stores it. Concurrent requests can all pass the initial check and trigger expensive generation simultaneously. This is a separate defect from the signature timing issue.

**Implement:** use per-account single-flight generation or pre-create and persist keys before publishing the actor. The atomic store operation must return the winning stored key and discard losing generated keys. Do not use this work package as a reason to force immediate Ed25519-only actor migration.

**Acceptance tests:**

- The production signing helper uses a blinded RSA private-key operation; no code path silently falls back to the unblinded `sign()` call.
- Legacy requests retain their existing Cavage-style RSA-SHA256 headers and are accepted by integration targets that support that profile, including Mastodon 4.4, 4.5/4.6, and 4.7, plus the selected Misskey/Sharkey test targets.
- Existing stored PKCS#8 PEM keys still load, actor key IDs remain stable, and signatures verify with the already-published public key.
- A provider-replacement test suite covers key generation/import, signing, public-key verification, malformed-key handling, and legacy interoperability.
- The dependency review accounts for every remaining RustCrypto `rsa` use. The advisory is not considered resolved until the vulnerable dependency is removed or a documented security review supports a deliberate exception; a blinding-only interim patch is not enough.
- Concurrent first fetches create one actor key and do not spawn a key-generation storm.

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

Do not call Birchtree production-ready until all of these are demonstrated on the release build:

- [ ] No untriaged high/critical security advisory; crypto plan documented and tested.
- [ ] Confirmation/reset bearers are non-predictable, protected at rest and removed/redacted after delivery; token consumption is atomic.
- [ ] Rate-limit client identity cannot be spoofed through untrusted forwarding headers.
- [ ] Existing blobs survive failed replacement; data/database/media directories have enforced permissions.
- [ ] File size and decoded-image limits are enforced; uploads and downloads are streamed; expensive processing is bounded and off the async executor.
- [ ] Durable and cache roots are truly separate; cache accounting restores after restart; eviction and reconciliation actually run.
- [ ] No staging files are publicly served; upload/metadata failures clean up or are reconciled.
- [ ] Background jobs have bounded class-specific concurrency, retention, redaction and reliable crash recovery.
- [ ] No network-facing RSA signer uses an unblinded private-key operation. AWS-LC-RS is the selected and tested RSA provider; RustCrypto RSA is absent from production dependency paths, or any remaining use is explicitly justified and documented. Legacy wire compatibility and any Ed25519/RFC 9421 policy are tested rather than assumed.
- [ ] Authorized-fetch and ordinary federation tests both pass for the target compatibility matrix.
- [ ] Restart/clock rollback cannot duplicate IDs; MFA lookup has an account index.
- [ ] Full CI, relevant smoke suites, Mastodon 4.7.3 compatibility tests, and backup/restore + realistic load tests pass.
- [ ] README, architecture map, current-state docs and TODOs describe the actual code and the remaining deviations accurately.

## Reference links

- Birchtree CI: https://github.com/estyseesghosts/birchtree/actions
- Current repository tree: https://github.com/estyseesghosts/birchtree/tree/master/src
- RustSec RSA/Marvin advisory: https://rustsec.org/advisories/RUSTSEC-2023-0071.html
- RustCrypto RSA signing/blinding API (interim mitigation only): https://github.com/RustCrypto/RSA/blob/master/src/key.rs
- AWS-LC-RS RSA provider docs: https://docs.rs/aws-lc-rs/latest/aws_lc_rs/signature/index.html
- RustSec `blurhash` advisory: https://rustsec.org/advisories/RUSTSEC-2023-0083.html
- RustSec `rustls-webpki` advisory: https://rustsec.org/advisories/RUSTSEC-2026-0382.html
- RustSec `webpki-roots` advisory: https://rustsec.org/advisories/RUSTSEC-2026-0381.html
- Mastodon 4.7 crypto/federation changes: https://blog.joinmastodon.org/2026/08/mastodon-4.7/
- Mastodon federation compatibility table: https://github.com/mastodon/mastodon/blob/main/FEDERATION.md
- RFC 9421 HTTP Message Signatures: https://www.rfc-editor.org/rfc/rfc9421.html
