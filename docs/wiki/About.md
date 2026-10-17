# About birchtree

birchtree is a Mastodon 4.7.3-compatible ActivityPub/SNS server written in Rust.

## Compatibility goal

A progressively tested drop-in replacement for Mastodon clients, targeting the v4.7.3 / API v7 contract; not a claim of complete API parity on day one.

## Initial deployment

- One Rust process
- `redb` embedded storage (single-node)
- Local media storage
- In-process durable-job workers

## Frontend

No application UI. A minimal server-rendered login/consent flow is required for Mastodon-compatible OAuth authorization. The first administrator account is bootstrapped by CLI.

## How to run

```bash
# Build and run the test suite
cargo check --all-targets
cargo test

# Run the full gate
./scripts/smoke-all.sh
```

The server binary serves the Phase 2 OAuth, registration and read APIs plus the CLI (`create-account`); see `docs/wiki/Current-State.md` for what is implemented.

## Design sources

- `birchtree.md` — plan, pinned to Mastodon v4.7.3 (commit `8ff11c2b`)
- `~/fedifedifedi/mastodon` — pinned Mastodon source (compatibility authority)
- `~/pinetree` — architecture/reference patterns only (not Misskey domain or API shapes)
