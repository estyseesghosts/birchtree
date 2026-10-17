# About birchtree

birchtree is a Mastodon 4.7.3-compatible ActivityPub/SNS server written in Rust.

## Compatibility goal

The bar: an unmodified Mastodon client connects to a birchtree instance and has an identical experience to connecting to Mastodon v4.7.3, measured at 98% or better, with the core flows matching in full. Deliberate differences are listed in the deviation register (`docs/compatibility-profile.md`). Parity is built up phase by phase, not claimed on day one.

## Initial deployment

- One Rust process
- `redb` embedded storage (single-node)
- Local media storage
- In-process durable-job workers

## Frontend

birchtree has no frontend source code in this repository. A minimal server-rendered login/consent flow is required for Mastodon-compatible OAuth authorization. The first administrator account is bootstrapped by CLI.

**keystodon** is the frontend, developed in a separate project in parallel. It runs in two modes. As the **first-party client**, birchtree serves its built bundle and keystodon uses birchtree's browser session with CSRF for `/api/web/*`. As a **third-party client**, keystodon authenticates against any host, including birchtree, through OAuth. keystodon is not on the development machine yet. It is the planned interoperability target for both modes.

## How to run

```bash
# Build and run the test suite
cargo check --all-targets
cargo test

# Run the full gate
./scripts/smoke-all.sh
```

The server binary serves the OAuth, registration, read, status, relationship, media, timeline and notification APIs (Phases 2–4) plus the CLI (`create-account`); see `docs/wiki/Current-State.md` for what is implemented.

## Design sources

- `birchtree.md` — plan, pinned to Mastodon v4.7.3 (commit `8ff11c2b`)
- `~/fedifedifedi/mastodon` — pinned Mastodon source (compatibility authority)
- `~/pinetree` — architecture/reference patterns only (not Misskey domain or API shapes)
