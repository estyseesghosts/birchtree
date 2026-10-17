# 2026-10-17 — Phase 4 (media slice): upload, processing lifecycle, file serving

## What changed

The MEDIA slice of Phase 4, implemented in a dedicated worktree at the
Phase-4-foundation commit. Scope: upload use cases, real image processing,
and the six media endpoints. No shared files were touched except
`Cargo.toml` (two new dependencies) and `docs/endpoint-manifest.md`.

**`src/media/processing.rs` (new implementation)**

- `process(mime_type, bytes) -> Result<Processed, ProcessError>`: decodes
  images (PNG/JPEG/GIF/WebP/BMP via the `image` crate, `default-features =
  false` + the five format features), reports `width`/`height`, encodes a
  `blurhash` (4x3 landscape / 3x4 portrait / 4x4 small, off the ≤460px
  preview pixels) and generates the `small` preview (fit in 460x460, never
  upscaled; re-encoded in the same format for JPEG/PNG/WebP, PNG for
  GIF/BMP).
- Video/audio: no transcoding; MP4/QuickTime dimensions parsed from the
  `tkhd` box (recursive ISO-BMFF walk; 16.16 fixed width/height at the box
  tail) else `None`; WebM/OGG/MP3/WAV/FLAC keep `None` (honest, never
  fabricated).
- `ProcessError::{Unidentifiable, Other}` reproduce Mastodon's two
  Paperclip rescue classes.

**`src/media/ingest.rs` (extended)**

- `ACCEPTED_MIME_TYPES`, `validate`, `is_large_format`, `is_image` plus unit
  tests; `sniff`/`is_emoji_image` unchanged.

**`src/application/media.rs` (replaces the placeholder)**

- `upload_media(...)`: blank check → content sniff (never the declared
  multipart type) → size cap → description ≤10000 chars → focus parse →
  thumbnail rules (video/audio only; must sniff `image/*`; within cap) →
  **bytes staged first** under `{aa}/{bb}/{random16}/original.{ext}` (key
  layout documented in-module: the id cannot appear in its own key because
  bytes precede the id-minting commit) → `store.create_media_upload` (row +
  `ProcessMedia` job in ONE transaction) → inline processing unless
  `delay && larger_media_format?` (Mastodon's `delay_processing?`).
- `process_media_job(...)`: idempotent (missing row / Ready / Failed →
  Ok); missing staged blob → retryable `Err`; undecodable bytes → row
  `Failed` with an "unsupported media type: …" reason, job still completes;
  preview bytes staged before the row commit.
- `update_media` / `show_media` / `delete_media`: owner-scoped,
  `where(status_id: nil)` semantics (attached/foreign → `Record not
  found`), `check_processing` (Failed → 422 processing error), in-usage
  delete rejection, best-effort blob cleanup after commit.
- `human_size` (Rails `number_to_human_size`) for the pinned
  "File must be less than 16 MB, …" message.

**`src/api/mastodon/endpoints/media.rs` (replaces the stubs)**

- `create` (200 inline), `create_v2` (200 Ready / 202 otherwise),
  `show` (200/206/422/404), `update` (multipart or form; 200/206/…),
  `destroy` (200 `{}` / 422 in-usage / 404), `serve_file` (bytes + sniffed
  content type + `Cache-Control: max-age=86400, public`, traversal-safe 404).
- All five API routes require `write | write:media` and a user token
  (Mastodon's `doorkeeper_authorize!` + `require_user!` on every action,
  `show` included); app-only token → 422 "This method requires an
  authenticated user".

**Tests** — `tests/phase4_media.rs` (8 integration tests through the real
router + multipart builder) and unit tests in `processing.rs` (7),
`ingest.rs` (+1), `application/media.rs` (7).

## Verified (executed)

- `cargo check --all-targets`: green, zero warnings.
- `cargo test`: 166 passed, 0 failed (all pre-existing suites plus the new
  media tests).
- `scripts/smoke-all.sh`: `smoke-all: green` (SMOKE_PHASE3_OK).

## Deliberate deviations (also listed in `docs/endpoint-manifest.md`)

- No ffmpeg: GIFs stay `image` (no `gifv`), video/audio bytes are served as
  uploaded, duration/frame rate/bitrate stay `null`.
- Blob keys shard by per-upload random hex, not attachment id (see above).
- Effective file-size cap = multipart body limit (`ApiState` carries no
  `Config`; `max_file_size ≤ max_body_size` is validated at startup).
- Non-numeric `focus` → 422 "Validation failed: Focus is invalid" (Ruby
  would coerce to `0.0`); blank focus is ignored on upload, clears on
  update.
- Image upload with a thumbnail answers "Thumbnail must be blank" before the
  thumbnail's own type/size checks (order differs from the model's
  validation order).

## Open questions / findings for integration

- **`dto.rs` bug (pre-existing, outside this slice's file set):**
  `media_meta_json` renders `meta.original.size` / `meta.small.size` as the
  literal string `"{width}x{height}"` instead of interpolating. Pinned by a
  test comment in `tests/phase4_media.rs`; needs a one-line fix in
  `src/api/mastodon/dto.rs` during integration.
- `MediaAttachment::url` (the row field) stays `None` forever — the DTO
  derives URLs from `file_key`; the field is vestigial.
- `server_public_url` is kept in the `upload_media` signature (underscored)
  for signature symmetry; the DTO builds all URLs from the committed key.
