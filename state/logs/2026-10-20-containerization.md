# 2026-10-20 — containerization guide (birchtree)

Scope: documentation only. No source change in this session. The deliverable
is `birchtree_container.md`, an implementation guide for packaging birchtree
as a container image in two variants (with and without ffmpeg).

## Goal

Produce the implementation guide an implementer needs to build birchtree
container images, including the runtime gotchas that are not visible from
the source alone.

## Reference findings (read, not assumed)

- **Health endpoints already exist.** `GET /health` (liveness, `200 ok`) and
  `GET /ready` (readiness, `200 ready` / `503 not ready`) are registered at
  `src/api/mastodon/router.rs:29-30` and covered by `tests/runtime.rs`
  (`health_is_ok`, `ready_reports_storage_available`). `ReadyCheck::is_ready`
  is a real redb read transaction (`src/persistence/redb/mod.rs:241`), not a
  constant. **No code change is needed for the health contract.**
- **ffmpeg is a startup gate, not an optional dependency.**
  `TranscodeConfig::validate()` probes the binary and requires
  `REQUIRED_ENCODERS = [libvpx-vp9, libopus, libwebp]` and 17 named
  `REQUIRED_DECODERS` (`src/media/transcode.rs:89-100`). A missing or
  incomplete ffmpeg is a **startup error**, never a silent passthrough.
  `transcoding_enabled` defaults to `true`, so an ffmpeg-less image must
  disable it — via the `--no-transcode` CLI flag, which overrides the config
  file, so an operator-supplied config cannot re-enable a missing pipeline.
- **The data dir must not be world-writable.** `prepare_data_dir()`
  (`src/persistence/redb/mod.rs:251`) rejects mode `& 0o002`. The server
  creates the dir `0700`; a pre-existing `0777` bind mount refuses to boot.
- **redb is single-writer** — one replica per volume, no scale-out.
- **`SIGTERM` is already handled** (`src/main.rs:353`, `shutdown_signal()`
  covers `SIGINT` and `SIGTERM`), so no shutdown change is needed here.
- **`vendor/activitypub_federation` is a git-tracked path dependency**
  (`Cargo.toml:59-60`, 74 files). The build context must include `vendor/`;
  a `.dockerignore` that excludes it breaks the build.
- The server links **OpenSSL** (`native-tls` via `lettre`/`reqwest`), so the
  runtime stage needs `libssl3`.
- `smtp_url` and `vapid_private_key` support `env:` indirection, so secrets
  can stay out of a mounted config file.
- `transcode_max_memory_bytes` defaults to **1 GiB** — the container needs
  headroom above that for the ffmpeg variant.

## Evidence

- **ffmpeg gate, Debian bookworm (`debian:bookworm-slim`, ffmpeg
  5.1.9-0+deb12u1), probed with the exact encoder/decoder lists from
  `src/media/transcode.rs`:** all 3 required encoders PRESENT
  (`libvpx-vp9`, `libopus`, `libwebp`) and all 17 required decoders PRESENT,
  including the native **`h264`** and **`hevc`**.
- **ffmpeg gate, Fedora 44 (`ffmpeg` from the distro), probed identically:**
  `libvpx-vp9`/`libopus`/`libwebp` PRESENT, but **`h264` and `hevc`
  MISSING** — Fedora ships only `libopenh264`, under a different name. A
  Fedora-based runtime image would **fail the gate and refuse to boot**.
  This is why the guide pins a Debian base.
- **The image in `birchtree_container.md` §6.2 was built and run.** The
  Dockerfile in the guide was extracted verbatim and built with
  `podman build --network=host --format docker` against the repo context.
  It builds unmodified. The running container:
  - logged `birchtree listening listen=0.0.0.0:3000
    public_url=http://localhost:3000 database=/data/birchtree.redb`;
  - answered `GET /health` → `ok` and `GET /ready` → `ready`;
  - served `GET /api/v2/instance` →
    `{"domain":"localhost","title":"Birchtree","version":"4.7.3
    (compatible; birchtree 0.1.0)",...}`;
  - stopped on `SIGTERM` with **exit code 0** and the log lines
    `job worker stopped` / `birchtree stopped`.
- **The ffmpeg gate was verified inside the built image** with the exact
  encoder/decoder lists from `src/media/transcode.rs`: all 3 required
  encoders PRESENT (`libvpx-vp9`, `libopus`, `libwebp`) and all 17 required
  decoders PRESENT, including the native `h264` and `hevc`. The image's
  ffmpeg is 5.1.9-0+deb12u1.

### Two defects the verification caught (both fixed in the guide)

1. **`USER` in the shared base stage broke the derived stage.** The first
   draft put `USER birchtree` in `runtime-base`. The `runtime` stage then
   inherited it and ran `apt-get install ffmpeg` as the unprivileged user,
   failing with `E: List directory /var/lib/apt/lists/partial is missing.
   - Acquire (13: Permission denied)`. **Fix:** `USER` now sits in the two
   final stages, after their `apt-get` and `COPY` steps; `runtime-base`
   stays root.
2. **Podman silently drops `HEALTHCHECK` in the default OCI image format**
   (`HEALTHCHECK is not supported for OCI image format and will be
   ignored`). `docker build` is unaffected, and a compose file that
   declares its own healthcheck is unaffected. The guide now says to build
   with `--format docker` on podman.
- **The `runtime-no-ffmpeg` target was built and run** too: ffmpeg is
  absent (`command -v ffmpeg` → not found), the container starts with no
  transcoding error in the logs, and it answers `GET /health` → `ok` and
  `GET /ready` → `ready`. It stops on `SIGTERM` with **exit code 0**.
- **Data persistence across a restart was verified**: an account created
  with `birchtree create-account --username=probe ... --role=admin` inside
  the container was still there after `podman restart`, and the CLI could
  still read the database.
- **Measured image sizes: ~572 MB with ffmpeg, ~115 MB without** — ffmpeg
  and its codec libraries are 457 MB of the difference.

## Changes

- **`birchtree_container.md`** (new, repo root): the implementation guide.
  Sections: the two image variants and why they exist; the runtime facts
  table; the full `Dockerfile` source (one file, two build targets:
  `runtime` with ffmpeg and `runtime-no-ffmpeg` without); the `.dockerignore`;
  the two runtime config files; the operator-config and secret-handling
  story; the health/readiness contract; the compose file; the runtime
  gotchas (data-dir permissions, redb lock, private-range federation, the
  optional keystodon bundle, streaming, memory); an executable verification
  checklist; a troubleshooting table; and security notes.
- **Contract bookkeeping (documentation gap, found by this work):** neither
  `/health` nor `/ready` is listed in `docs/endpoint-manifest.md`, although
  the manifest is the endpoint contract and `AGENTS.md` requires every route
  to be pinned there. The manifest already has a section for public routes
  outside upstream's `config/routes/api.rb`; the guide gives the two rows to
  add there. Pre-existing omission; no code change involved.

## Open questions / next step

- The guide is a proposal: the `Dockerfile`, `.dockerignore`,
  `docker/*.json` and `compose.yaml` do **not** exist yet. The guide gives
  their full contents, so the next step is a mechanical transcription plus
  the §11 verification pass.
- Not verified yet (needs an image build): the exact image sizes, the
  multi-stage build's wall-clock time, and whether the `runtime-no-ffmpeg`
  target starts with `--no-transcode` overriding an operator-replaced config
  file. §11.3 and §11.4 are the checks.
- `docker compose` was not used to verify the compose file: the host has
  `podman` 5.8.9 and `podman-compose` but no `docker` daemon, so the compose
  file's syntax was written against the compose spec rather than executed.
  §11 uses plain `podman` commands for that reason.
- The `web_client_dir` (keystodon bundle) path is documented but untested in
  a container; it needs its own volume mount.
