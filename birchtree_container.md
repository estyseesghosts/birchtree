# birchtree — containerization guide

> **Status:** proposal / implementation guide. Nothing in this file is applied
> to the tree except where a step says *"already applied"*.
> **Audience:** an implementer picking up the container work for birchtree.
> **Scope:** container images for the `birchtree` server — **two variants**,
> one with the ffmpeg transcode pipeline and one without — plus the health
> probe contract and the runtime gotchas.
>
> Source of truth for the product itself remains `birchtree.md`; this
> document does not change the API or storage contracts.

---

## 1. Summary

`birchtree` is a modular Mastodon-compatible SNS server: a **single Rust
binary** with an **embedded** `redb` store and a filesystem blob store.
There is no database server, no message broker and no asset pipeline.

The container work is straightforward **except for one thing**: birchtree
**refuses to start without a working ffmpeg** unless transcoding is turned
off. ffmpeg is not a library dependency — it is an external process the
server spawns for the Phase 7.1 media pipeline, and the server *probes it at
startup* and requires a specific set of encoders and decoders. That single
requirement drives the whole image design, which is why the guide ships
**two variants**:

| Image | Target | ffmpeg | Media behaviour |
|-------|--------|--------|-----------------|
| `birchtree:latest` | `runtime` | **yes** | Full transcode pipeline: WebP stills, VP9 video, Opus audio |
| `birchtree:no-ffmpeg` | `runtime-no-ffmpeg` | **no** | Passthrough (Phase 4 behaviour): uploads are stored as uploaded |

Everything else — the non-root user, the data volume, the health probes,
the compose file — is standard.

**Effort estimate:** half a day, most of it in the verification pass, because
the ffmpeg gate is easy to get subtly wrong (see §6.3).

---

## 2. Deliverables

Create these files:

```
birchtree/
├── Dockerfile                    # multi-stage; two runtime targets
├── .dockerignore                 # build-context hygiene
├── docker/
│   ├── birchtree.container.json       # runtime config WITH transcoding
│   ├── birchtree.container.no-ffmpeg.json
│   └── birchtree.local.json           # operator config (sample)
├── compose.yaml                  # single-node deployment
└── birchtree_container.md        # this file
```

**No source changes are required.** The health and readiness endpoints
already exist (§7).

---

## 3. Prerequisites

| Tool | Version | Why |
|------|---------|-----|
| Rust | **≥ 1.85** | `Cargo.toml` uses `edition = "2024"` |
| Docker / Podman | any recent | image build + run |
| C compiler (`cc`) | any | `ring 0.17` compiles C at build time |
| OpenSSL headers (`libssl-dev`) | 1.1 or 3 | `openssl-sys` via `native-tls` (`lettre`, `reqwest`) |

There is **no** `rust-toolchain.toml` in the repo, so pin the version in the
Dockerfile yourself (§6.2) rather than relying on `rust:latest`.

### 3.1 The vendored dependency

`Cargo.toml` patches `activitypub_federation` to a **path dependency**:

```toml
[patch.crates-io]
activitypub_federation = { path = "vendor/activitypub_federation" }
```

`vendor/` is git-tracked (74 files) and carries the five upstream patches
listed in `vendor/README.md` — including the 1 MiB inbox limit and the
Akkoma `publicKeyPem` newline fix. **The build context must include
`vendor/`**; a `.dockerignore` that excludes it breaks the build. The
patch is also why `cargo build --locked` must be used: `Cargo.lock` already
records the patched dependency.

---

## 4. Runtime facts

| Fact | Consequence for the image |
|------|---------------------------|
| **ffmpeg is a startup gate.** `TranscodeConfig::validate()` probes the binary and requires specific encoders/decoders; a missing or broken ffmpeg is a **startup error**, never a silent passthrough | The image must ship an ffmpeg that satisfies the gate, or disable transcoding. See §6.3 |
| `transcoding_enabled` defaults to **`true`** | The ffmpeg-less image must disable it explicitly (§6.4) |
| `--no-transcode` CLI flag exists and **overrides** the config file | Use the flag, not the config key, so an operator-supplied config cannot re-enable a missing pipeline |
| **The data dir must not be world-writable.** `prepare_data_dir()` rejects mode `& 0o002` | `/data` must be `0700` and owned by the runtime uid; a `0777` bind mount refuses to boot |
| **redb takes an exclusive lock** on the database file | One replica per volume. No scale-out |
| The server links **OpenSSL** (`native-tls`) | The runtime image needs `libssl3` |
| `smtp_url` and `vapid_private_key` support **`env:` indirection** | Secrets can stay out of the mounted config file |
| Logging is **`tracing`** with `RUST_LOG` | Standard stdout log driver works |
| `max_file_size` / `max_body_size` default to **16 MiB** | Modest memory needs, unless transcoding |
| `transcode_max_memory_bytes` defaults to **1 GiB** | The ffmpeg variant needs **> 1 GiB** of memory headroom |

---

## 5. The two variants

### 5.1 With ffmpeg — the default

The full media pipeline. Uploads are decoded and re-encoded: stills become
WebP (`libwebp`), video becomes VP9 (`libvpx-vp9`) with Opus audio
(`libopus`). This is what a production instance wants.

**Measured image size: ~572 MB** (built from the guide's Dockerfile).

### 5.2 Without ffmpeg — `runtime-no-ffmpeg`

Transcoding is disabled and uploads are **stored as uploaded** (the Phase 4
passthrough behaviour: no derived stills, no transcoded video). This is
useful when:

- you want a **small image** — ffmpeg and its codec libraries are **457 MB
  of the image**: the ffmpeg-less variant measures **~115 MB**, a fifth of
  the size;
- the instance serves no media-heavy traffic;
- you want to run the server on a minimal host.

What you lose: the derived media pipeline (Phase 7.1), the WebP/VP9/Opus
derivatives, and the orphan sweeper's transcode staging cleanup (the
`sweep-transcode-orphans` subcommand still runs, it just finds nothing).

**What you do not lose:** every other subsystem — federation, streaming,
search, moderation, the admin API. Transcoding is a post-commit job
(`src/media/transcode.rs`); with it off, the upload path routes around the
module entirely.

**Switching between them at runtime is not supported**: turning transcoding
on requires ffmpeg in the image, and the startup gate will reject an image
without it.

---

## 6. The image

### 6.1 `.dockerignore`

```dockerignore
# Build artefacts
/target/
/data/

# Version control and CI
/.git/
/.github/

# Logs, bench output, local scratch
/state/
*.log
.env
.env.local

# Reference checkouts (git-ignored symlinks; large)
pinetree
mastodon
```

> **Do not exclude `vendor/`** — the build needs the patched
> `activitypub_federation` (§3.1). Also keep `src/`, `Cargo.toml` and
> `Cargo.lock`.

### 6.2 `Dockerfile`

> **Build from the repository root.** The Dockerfile copies `Cargo.toml`,
> `Cargo.lock`, `vendor/` and `src/` from the build context, so the context
> must be the repository root:
>
> ```bash
> podman build -t birchtree:latest .
> ```

```dockerfile
# syntax=docker/dockerfile:1.7
#
# birchtree — Mastodon-compatible SNS server.
#
#   docker build -t birchtree:latest .                        # with ffmpeg
#   docker build --target runtime-no-ffmpeg \
#       -t birchtree:no-ffmpeg .                              # without ffmpeg
#
# Debian (not Alpine) is deliberate, for two reasons:
#
#   1. birchtree links against OpenSSL (native-tls via lettre and reqwest);
#      Alpine's musl/OpenSSL combination is a different ABI and an
#      unsupported configuration for this server.
#   2. The ffmpeg startup gate (§6.3) requires the native `h264` and `hevc`
#      decoders. Debian's ffmpeg carries them; Alpine's and Fedora's do
#      not, and a build missing them will not boot.
#
# See birchtree_container.md §4 for the runtime facts and §11 for the
# verification checklist.

ARG RUST_VERSION=1.99
ARG DEBIAN_RELEASE=bookworm

# ---- builder ---------------------------------------------------------------
FROM rust:${RUST_VERSION}-${DEBIAN_RELEASE} AS build

# `ring` compiles C; `openssl-sys` needs the OpenSSL headers and pkg-config.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        libssl-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src

# Dependency layer: only the manifests and the vendored patch are copied, so
# an edit to src/ does not re-resolve and re-compile the whole dependency
# graph. `vendor/` is required: Cargo.toml patches activitypub_federation
# to a path dependency (see vendor/README.md).
COPY Cargo.toml Cargo.lock ./
COPY vendor/ ./vendor/

# Stub the two crate roots so cargo resolves and builds the dependency graph
# once. The real sources replace the stubs in the next layer.
RUN mkdir -p src \
    && printf 'fn main() {}\n' > src/main.rs \
    && : > src/lib.rs \
    && cargo build --release --locked \
    && rm -rf src

COPY src/ ./src/

# The stub artefacts are already in target/; force a rebuild of the crate
# itself only, not of the dependencies.
RUN touch src/main.rs src/lib.rs \
    && cargo build --release --locked \
    && strip target/release/birchtree \
    && install -Dm755 target/release/birchtree /out/birchtree

# ---- runtime base ----------------------------------------------------------
FROM debian:${DEBIAN_RELEASE}-slim AS runtime-base

# ca-certificates → TLS roots for outbound federation and SMTP
# libssl3         → the OpenSSL runtime `lettre`/`reqwest` link against
# curl            → used by the HEALTHCHECK probe (the server itself uses
#                   reqwest, not curl)
# tini            → PID 1: reaps children and forwards signals to the server
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        libssl3 \
        tini \
    && rm -rf /var/lib/apt/lists/*

ARG APP_UID=10001
ARG APP_GID=10001

RUN groupadd --system --gid ${APP_GID} birchtree \
    && useradd --system --uid ${APP_UID} --gid ${APP_GID} \
        --home-dir /app --shell /usr/sbin/nologin birchtree \
    && mkdir -p /app /data \
    && chown -R ${APP_UID}:${APP_GID} /app /data \
    && chmod 700 /data

WORKDIR /app

# The internal listener is fixed at 3000 so the healthcheck, EXPOSE and the
# compose file cannot drift apart. Publish it as you like: `-p 8080:3000`.
EXPOSE 3000

# Liveness. `curl` is already in the image, so no extra package is needed.
# Readiness is available at /ready for orchestrators that need it.
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD curl -fsS http://127.0.0.1:3000/health || exit 1

# tini is PID 1: it forwards SIGTERM to the server (which drains) and reaps
# any child the server leaves behind.
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/birchtree"]

# --data-dir and --listen are passed as flags so they survive an operator
# replacing /etc/birchtree/config.json with their own file (CLI flags
# override file values).
CMD ["--config=/etc/birchtree/config.json", "--data-dir=/data", "--listen=0.0.0.0:3000"]

VOLUME ["/data"]

# ---- variant: without ffmpeg ----------------------------------------------
# Small image, no transcode pipeline. `--no-transcode` is passed as a CLI
# flag (not only as a config key) so that an operator replacing the config
# file cannot accidentally re-enable a pipeline whose ffmpeg is not in the
# image — the startup gate would refuse to boot.
#
# `USER` is set in the final stages, not in `runtime-base`: a stage that
# inherits `runtime-base` still has to run `apt-get` as root, and a
# `USER` in the base would break that with
# "List directory /var/lib/apt/lists/partial is missing - Permission denied".
FROM runtime-base AS runtime-no-ffmpeg

COPY docker/birchtree.container.no-ffmpeg.json /etc/birchtree/config.json
COPY --from=build /out/birchtree /usr/local/bin/birchtree

# Drop to the unprivileged user before the server starts.
USER birchtree

CMD ["--config=/etc/birchtree/config.json", "--data-dir=/data", "--listen=0.0.0.0:3000", "--no-transcode"]

# ---- variant: with ffmpeg (default build target) --------------------------
# The ffmpeg package is installed last so the layer stays cached when only
# the source changes. It must satisfy the startup gate in §6.3.
FROM runtime-base AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ffmpeg \
    && rm -rf /var/lib/apt/lists/*

COPY docker/birchtree.container.json /etc/birchtree/config.json
COPY --from=build /out/birchtree /usr/local/bin/birchtree

# Drop to the unprivileged user before the server starts.
USER birchtree
```

### 6.3 The ffmpeg startup gate

This is the part that bites. `src/media/transcode.rs` probes ffmpeg at
startup and **fails the boot** if the toolchain is missing or incomplete:

```rust
/// Encoders the startup check requires (`ffmpeg -encoders` names).
pub const REQUIRED_ENCODERS: &[&str] = &["libvpx-vp9", "libopus", "libwebp"];

/// Decoders the startup check requires (`ffmpeg -decoders` names).
pub const REQUIRED_DECODERS: &[&str] = &[
    "png", "mjpeg", "gif", "bmp", "webp", "tiff", "h264", "hevc", "mpeg4", "vp8", "vp9",
    "aac", "opus", "vorbis", "mp3", "flac", "pcm_s16le",
];
```

The probe runs `ffmpeg -version` (the output must contain `ffmpeg version`),
then `ffmpeg -encoders` and `ffmpeg -decoders`, and refuses to start if any
required name is missing.

**Consequences for image construction:**

- The image must ship an ffmpeg whose `-encoders`/`-decoders` output contains
  **all 20 names**. Debian bookworm's `ffmpeg` package carries them.
- **Fedora's ffmpeg does not.** Fedora builds ffmpeg without the native
  `h264` and `hevc` decoders (only `libopenh264`, under a different name),
  so a Fedora-based image **will not boot** with transcoding enabled. This
  is the single most likely way to get this wrong: it looks fine until the
  server refuses to start.
- Alpine's ffmpeg is also not a safe assumption, and Alpine is ruled out
  anyway by the OpenSSL requirement (§6.2).

**Always verify the gate against the built image** — §11.3 has the exact
command. Do not assume the distro package satisfies it.

> **Verified (2026-10-20).** Debian bookworm's `ffmpeg` package
> (**5.1.9-0+deb12u1**) was probed with the exact encoder/decoder lists
> above and satisfies the gate completely: `libvpx-vp9`, `libopus` and
> `libwebp` are present as encoders, and all 17 required decoders —
> including the native **`h264`** and **`hevc`** — are present. Fedora 44's
> `ffmpeg`, probed on the same machine, has **no** native `h264` or `hevc`
> decoder (only `libopenh264`, under a different name), so a
> Fedora-based runtime image would fail the gate and refuse to boot.

### 6.4 Runtime config

`docker/birchtree.container.json` (the **with-ffmpeg** variant):

```json
{
  "public_url": "http://localhost:3000",
  "data_dir": "/data",
  "listen": "0.0.0.0:3000",
  "site_title": "Birchtree",
  "site_description": "A Mastodon-compatible server",
  "site_contact_email": "admin@localhost",
  "registration_open": false,
  "registration_requires_approval": false,
  "search_mode": "embedded",
  "transcoding_enabled": true
}
```

`docker/birchtree.container.no-ffmpeg.json` is the same file with
`transcoding_enabled` flipped to `false`:

```json
{
  "public_url": "http://localhost:3000",
  "data_dir": "/data",
  "listen": "0.0.0.0:3000",
  "site_title": "Birchtree",
  "site_description": "A Mastodon-compatible server",
  "site_contact_email": "admin@localhost",
  "registration_open": false,
  "registration_requires_approval": false,
  "search_mode": "embedded",
  "transcoding_enabled": false
}
```

(The image also passes `--no-transcode` on the command line, which
overrides this key — see §6.2.)

Notes:

- **Every key is optional.** `Config` is `#[serde(default, deny_unknown_fields)]`,
  so `{}` parses and takes the compiled defaults. Unknown keys are a **hard
  error** — a typo in a mounted config stops the container at boot.
- **`public_url` must be set to the externally visible URL.** The compiled
  default is `http://localhost:3000`. Every actor URI, WebFinger answer and
  NodeInfo document is built from it, so a wrong value breaks federation.
- **`registration_open` defaults to `false`** in the compiled config.
  Whether to open registration is an operator decision, not a container
  default — leave it `false` and let the operator flip it.
- **`transcoding_enabled`** in the config is a convenience; the
  no-ffmpeg image also passes `--no-transcode` (§6.2) so the flag wins even
  if the config file is replaced.
- **`search_mode`** is one of `embedded` (default), `database` or `off`.

### 6.5 Operator config

Operators override by mounting their own file over
`/etc/birchtree/config.json`:

```bash
docker run -d --name birchtree \
  -p 3000:3000 \
  -v birchtree-data:/data \
  -v "$PWD/docker/birchtree.local.json:/etc/birchtree/config.json:ro" \
  birchtree:latest
```

Because `CMD` passes `--data-dir` and `--listen` as flags, a minimal operator
file only needs the keys it wants to change:

```json
{ "public_url": "https://social.example.com" }
```

**Secrets** stay out of the file. birchtree resolves `env:` indirections on
the two secret-bearing fields (`smtp_url`, `vapid_private_key`):

```json
{
  "public_url": "https://social.example.com",
  "smtp_url": "env:BIRCHTREE_SMTP_URL",
  "vapid_private_key": "env:BIRCHTREE_VAPID_PRIVATE_KEY"
}
```

```bash
docker run -d \
  -e BIRCHTREE_SMTP_URL='smtps://user:pass@mail.example.com:587' \
  -e BIRCHTREE_VAPID_PRIVATE_KEY='...' \
  ...
```

An unset referenced variable is a **hard error** naming the variable — a
missing secret stops the container rather than silently disabling mail.

---

## 7. Health and readiness endpoints

**No code change is needed.** Both endpoints already exist and are already
covered by tests.

| Endpoint | Status | Semantics |
|----------|--------|-----------|
| `GET /health` | **exists** | Liveness. `200` with the body `ok` while the process is alive. Registered at `src/api/mastodon/router.rs:29`. |
| `GET /ready` | **exists** | Readiness. `200` `ready` when storage can serve a read, `503` `not ready` otherwise. Registered at `src/api/mastodon/router.rs:30`. |

```rust
/// Liveness probe: the process is running.
async fn health() -> &'static str {
    "ok"
}

/// Readiness probe: storage is open and can serve a read.
async fn ready(State(state): State<ApiState>) -> (StatusCode, &'static str) {
    if state.context.store.is_ready() {
        (StatusCode::OK, "ready")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready")
    }
}
```

`ReadyCheck::is_ready` is implemented by the redb adapter as
`self.db.begin_read().is_ok()` (`src/persistence/redb/mod.rs:241`) — a real
read transaction against the live database, not a constant.

Existing coverage: `tests/runtime.rs` has `health_is_ok` and
`ready_reports_storage_available`. Keep them green.

**Contract bookkeeping (pre-existing gap).** Neither probe appears in
`docs/endpoint-manifest.md`, even though the manifest is the endpoint
contract and `AGENTS.md` requires every route to be pinned there. The
manifest already has a section for public routes outside upstream's
`config/routes/api.rb` ("OAuth / discovery"), and the probes belong there.
Add two rows to that section:

| Method | Path | Auth | Scope | Status | Profile | Notes |
|---|---|---|---|---|---|---|
| GET | `/health` | none | — | implemented | In | Liveness probe: `200` `ok` while the process is alive |
| GET | `/ready` | none | — | implemented | In | Readiness probe: `200` `ready` when storage can serve a read, `503` `not ready` otherwise |

This is a one-line-each documentation change, not a code change, and it
closes the gap the container work exposes.

**Contract to preserve:**

| Method | Path | Success | Failure |
|--------|------|---------|---------|
| `GET` | `/health` | `200` `ok` | — |
| `GET` | `/ready` | `200` `ready` | `503` `not ready` |

Both are unauthenticated and outside `/api/`, so they are not affected by
the API rate limiter or the JSON body guard.

---

## 8. `compose.yaml`

```yaml
name: birchtree

services:
  birchtree:
    build:
      context: .
      dockerfile: Dockerfile
      target: runtime
    image: birchtree:latest
    container_name: birchtree
    restart: unless-stopped

    # The internal port is fixed at 3000; publish it wherever you like.
    ports:
      - "127.0.0.1:3000:3000"

    volumes:
      - birchtree-data:/data
      # Mount your own config over the baked-in one. The keys you do not
      # set fall back to the compiled defaults; unknown keys are a hard
      # error at boot.
      - ./docker/birchtree.local.json:/etc/birchtree/config.json:ro

    # Give the server time to drain in-flight requests, federation
    # deliveries and streaming connections before SIGKILL. The default 10s
    # is tight for a busy node.
    stop_grace_period: 30s

    environment:
      # Secrets referenced with `env:` in the config file.
      # - BIRCHTREE_SMTP_URL=smtps://...
      # - BIRCHTREE_VAPID_PRIVATE_KEY=...
      # Logging filter for `tracing` (default: info).
      - RUST_LOG=info

    # The transcode pipeline allows a 1 GiB RSS per job by default
    # (`transcode_max_memory_bytes`). Give the container headroom above
    # that, or lower the cap in the config.
    mem_limit: 1536m

    logging:
      driver: json-file
      options:
        max-size: "10m"
        max-file: "3"

volumes:
  birchtree-data:
```

**The no-ffmpeg variant** is the same file with:

```yaml
    build:
      context: .
      dockerfile: Dockerfile
      target: runtime-no-ffmpeg
    image: birchtree:no-ffmpeg
```

and a lower memory limit (`mem_limit: 512m` is enough without transcoding),
because the 1 GiB per-job transcode cap does not apply.

**Reverse proxy.** If the server sits behind nginx/Traefik, set
`trusted_proxies` in the config to the proxy's address so `X-Forwarded-For`
is honoured. The JSON key is `trusted_proxies` (plural); the CLI flag is
`--trusted-proxy=` (singular).

**Bootstrapping the first admin.** The image has no entrypoint script, so
create the account with an explicit one-shot run. `--role=admin` grants the
admin flag at creation time:

```bash
docker compose run --rm birchtree create-account \
  --username=admin \
  --password='<strong>' \
  --email='admin@example.com' \
  --role=admin
```

`--email` is optional but recommended (password resets and confirmations go
to it). `--password` may also be supplied as the `BIRCHTREE_PASSWORD`
environment variable, which keeps it out of the process table — useful with
a Docker secret:

```bash
docker compose run --rm -e BIRCHTREE_PASSWORD \
  birchtree create-account --username=admin --email='admin@example.com' --role=admin
```

The role can also be set after the fact. The subcommand is spelled
`set-role` or `grant-role`, and it takes **`--role=`**, not `--admin`:

```bash
docker compose run --rm birchtree set-role --username=admin --role=admin
```

(`--role=` accepts `admin`, `moderator` or `none`.)

---

## 9. Runtime gotchas

### 9.1 The data directory must not be world-writable

`prepare_data_dir()` (`src/persistence/redb/mod.rs:251`) refuses to open
storage when the data dir is world-writable:

```rust
if mode & 0o002 != 0 {
    return Err(StoreError::Migration(format!(
        "data directory {} is world-writable (mode {:o}); refusing to open storage",
        ...
    )));
}
```

The server creates the directory `0700` if it does not exist, but a
**pre-existing** `0777` directory (a careless bind mount, or a `tmpfs` with
the wrong options) is rejected. The image pre-creates `/data` as `0700`
owned by uid 10001; a named volume inherits that. A bind mount must be
`chown`ed to the runtime uid and must not be world-writable.

### 9.2 redb is single-writer

`redb` takes an **exclusive lock** on `birchtree.redb`. Only one process
may open it. Do not run two replicas against the same volume, and do not
declare `deploy.replicas` in compose.

### 9.3 The data volume

```
/data/birchtree.redb     # the embedded database
/data/media/...          # uploaded media, plus the transcode staging area
```

Transcode jobs stage temporary files under the system temp dir
(`/tmp`), so keep `/tmp` writable — it is, by default.

### 9.4 Private-range federation is blocked by default

The federation client rejects private and reserved IP ranges by default
(`federation_allowlist` is empty, `federation_allow_http` is off). In a
compose network every container has a private address, so
**container-to-container federation is refused** unless you set:

```bash
--federation-allow-private=172.28.0.0/16   # your compose network CIDR
--federation-allow-http                    # only if the peers are plain HTTP
```

These are lab switches, mirroring Mastodon's `ALLOWED_PRIVATE_ADDRESSES`.
Never enable them on a public instance.

### 9.5 The optional web client bundle

`web_client_dir` points at a keystodon-style first-party web bundle
served under `/app`. It is `None` by default, in which case `/app/*` answers
404. If you want it, mount the bundle and set the key:

```json
{ "web_client_dir": "/srv/web-client" }
```

### 9.6 Streaming

Streaming (SSE and WebSocket) is served from the **same listener** on the
same port. `streaming_api_base_url` is for the opposite case — a separate
streaming host — in which case the streaming paths on this host answer
`301` to that host.

### 9.7 Memory

The transcode pipeline enforces a per-job RSS cap of **1 GiB** by default
(`transcode_max_memory_bytes`), polled from `/proc/<pid>/status` on Linux.
Size the container above that, or lower the cap in the config. The
no-ffmpeg variant has no such requirement.

---

## 10. Health probe wiring

| Concern | Endpoint | Behaviour |
|---------|----------|-----------|
| Container `HEALTHCHECK` | `GET /health` | `200 ok` while the process is alive |
| Orchestrator readiness | `GET /ready` | `200 ready` when storage is readable, `503 not ready` otherwise |
| Compose healthcheck | `GET /health` | same as the image `HEALTHCHECK` |

The image's `HEALTHCHECK` is inherited by compose, so `compose.yaml` does
not need to redefine it.

---

## 11. Verification checklist

Every item is executable; paste the real output into the session log per
`AGENTS.md`.

### 11.1 Build both variants

```bash
podman build -t birchtree:latest .
podman build --target runtime-no-ffmpeg -t birchtree:no-ffmpeg .
podman images birchtree
```

> **Podman only:** podman's default **OCI** image format silently **drops
> `HEALTHCHECK`** (`HEALTHCHECK is not supported for OCI image format and
> will be ignored`). Build with `--format docker` if you rely on the
> image's healthcheck:
>
> ```bash
> podman build --format docker -t birchtree:latest .
> ```
>
> `docker build` has no such problem. The compose file defines its own
> healthcheck, so compose deployments are unaffected either way.

### 11.2 The runtime has what the server needs

```bash
podman run --rm --entrypoint sh birchtree:latest -c \
  'command -v tini && command -v curl && ls -ld /data && id birchtree'
```

`/data` must be owned by uid 10001 with mode `0700`.

### 11.3 **The ffmpeg gate** (the critical check)

Run the same probe the server runs at startup, against the built image:

```bash
podman run --rm --entrypoint sh birchtree:latest -c '
ffmpeg -hide_banner -version | head -1
for e in libvpx-vp9 libopus libwebp; do
  printf "%-14s " "$e"
  ffmpeg -hide_banner -encoders 2>/dev/null | grep -q "$e" && echo PRESENT || echo MISSING
done
for d in png mjpeg gif bmp webp tiff h264 hevc mpeg4 vp8 vp9 aac opus vorbis mp3 flac pcm_s16le; do
  printf "%-12s " "$d"
  ffmpeg -hide_banner -decoders 2>/dev/null | grep -qE "^ [A-Z.]{6} +$d " && echo PRESENT || echo MISSING
done'
```

**All 20 names must print PRESENT.** Anything else means the container will
refuse to boot — check the logs for `transcoding: ...`.

### 11.4 The no-ffmpeg variant has no ffmpeg, and starts anyway

```bash
podman run --rm --entrypoint sh birchtree:no-ffmpeg -c \
  'command -v ffmpeg || echo "ffmpeg absent (expected)"'
```

Then boot it (§11.5) and confirm it starts without transcoding.

### 11.5 The server starts and answers the probes

```bash
podman run -d --name birchtree-test \
  -p 127.0.0.1:3000:3000 \
  -v birchtree-test-data:/data \
  birchtree:latest

sleep 5
curl -fsS http://127.0.0.1:3000/health   # -> ok
curl -fsS http://127.0.0.1:3000/ready    # -> ready
curl -fsS http://127.0.0.1:3000/api/v2/instance | head -c 200
```

For the ffmpeg variant, the logs must **not** contain a `transcoding:`
error. For the no-ffmpeg variant, the logs must show it starting
normally.

### 11.6 The container healthcheck goes healthy

```bash
podman inspect --format '{{.State.Health.Status}}' birchtree-test
# -> healthy
```

### 11.7 `SIGTERM` stops it cleanly

birchtree already handles `SIGTERM` (`src/main.rs:353`, `shutdown_signal()`
covers both `SIGINT` and `SIGTERM`), so this should pass with no code
change — but it is worth pinning in a container context:

```bash
podman stop --time 30 birchtree-test
podman inspect --format '{{.State.ExitCode}}' birchtree-test
# -> 0   (137 means SIGKILL: the drain timed out)
podman logs birchtree-test | grep -i 'birchtree stopped'
```

### 11.8 Data survives a restart

```bash
podman exec birchtree-test birchtree create-account \
  --username=probe --password=probepass --email=probe@example.com
podman restart --time 30 birchtree-test
sleep 5
curl -fsS http://127.0.0.1:3000/ready
```

The account must still be there.

### 11.9 A media upload works (ffmpeg variant only)

```bash
curl -fsS -X POST http://127.0.0.1:3000/api/v1/media \
  -H 'Authorization: Bearer <token>' \
  -F 'file=@/path/to/a.png'
```

Then confirm the transcode job ran and did not quarantine the attachment.

### 11.10 The full project gate still passes

```bash
bash scripts/smoke-all.sh
```

### 11.11 Cleanup

```bash
podman rm -f birchtree-test
podman volume rm birchtree-test-data
```

---

## 12. Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| Boot fails with `transcoding: ...` | The image's ffmpeg is missing a required encoder/decoder | §6.3 and §11.3. On Fedora-based images the native `h264`/`hevc` decoders are absent — use the Debian base |
| Boot fails with `ffmpeg binary "ffmpeg" could not be executed` | No ffmpeg in the image, and transcoding is on | Use `birchtree:no-ffmpeg`, or add ffmpeg to the image |
| Boot fails with `data directory ... is world-writable` | The volume is `0777` | `chown 10001:10001` it and `chmod 700` |
| Boot fails with `unknown field` in the config | `deny_unknown_fields` and a typo | Fix the JSON. Note `trusted_proxies` (config) vs `--trusted-proxy=` (flag) |
| The published port answers nothing | `listen` is not `0.0.0.0` | The `CMD` flag pins it; rebuild the image |
| The container is `unhealthy` right after boot | The first redb migration is slow | Raise `HEALTHCHECK --start-period` to 30s |
| Federation deliveries fail to peers on the compose network | SSRF private-range rejection | `--federation-allow-private=<compose CIDR>` (lab only) |
| The transcode job quarantines every upload | ffmpeg present but incomplete | Re-run §11.3; the missing codec is named in the startup error |
| `docker stop` takes the full grace period, exit code 137 | The drain did not finish | Raise `stop_grace_period`; check for stuck streaming connections |
| The image is very large | ffmpeg and its codec libraries | Use the `runtime-no-ffmpeg` target |

---

## 13. Security notes

- The server runs as **uid 10001**, not root. `/data` is `0700`.
- `tini` is PID 1, so the server is a child process and receives forwarded
  signals; it also reaps the ffmpeg children the transcode pipeline spawns.
- The runtime stage ships **no compiler and no source**: the stripped binary
  only. (The builder stage does have a compiler — it is not in the final
  image.)
- Consider `read_only: true` plus a `tmpfs` for `/tmp` in production: the
  transcode pipeline stages temporary files under the system temp dir, and
  the blob store's staging area lives under `/data`.
- `no-new-privileges` and dropping all capabilities are safe here — the
  server binds an unprivileged port and needs no special privileges.
- Do not mount the host Docker socket, and do not run the container with
  `--privileged`.
- Keep `--federation-allow-private` and `--federation-allow-http` unset in
  production: they disable the SSRF protection that stops federation from
  reaching internal services, and the second one allows plaintext
  federation.
- `RUST_LOG=debug` (or `birchtree::federation=debug`) prints untrusted
  inbound data. Keep it at `info` outside debugging.
- The optional keystodon web bundle is served from the same origin as the
  API — review it before deploying.

---

## 14. Out of scope

- **Multiple replicas.** redb is single-writer (§9.2).
- **A metrics endpoint.** birchtree has no Prometheus endpoint today; the
  health and readiness probes are the only operational surface.
- **Automatic database backups.** A named volume is not a backup; snapshot
  the volume or back up `<data_dir>` externally.
- **TLS termination.** The server speaks plain HTTP; put a reverse proxy in
  front and set `trusted_proxies`.
- **Migrating an existing bare-metal instance into a container.** Copy
  `<data_dir>` into the volume, keeping the ownership and the `0700` mode.
- **Distributing the image to a registry.** The build produces a local
  image; publishing it is a separate decision (signing, tags, provenance).
