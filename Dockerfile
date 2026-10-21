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
