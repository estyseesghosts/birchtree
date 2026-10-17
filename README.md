# birchtree

A Mastodon 4.7.3-compatible ActivityPub/SNS server in Rust.

**Compatibility goal:** a progressively tested drop-in replacement for Mastodon clients, targeting the v4.7.3 / API v7 contract; not a claim of complete API parity on day one.

**Initial deployment:** one Rust process, `redb`, local media storage, in-process durable-job workers. No application UI — a minimal server-rendered login/consent flow is required for Mastodon-compatible OAuth authorization.

## References

- `birchtree.md` — revised implementation plan (pinned to Mastodon v4.7.3, commit `8ff11c2b`).
- `AGENTS.md` — rules for agents working in this repo.
- `docs/` — frozen and draft contracts.
- `docs/wiki/` — living overview.
- `state/logs/` — dated work logs.
- `~/fedifedifedi/mastodon` — pinned Mastodon v4.7.3 source (compatibility authority).
- `~/pinetree` — architectural reference patterns (not Misskey domain or API shapes).

## Quick start

```bash
cargo check --all-targets
cargo test
./scripts/smoke-all.sh
```

See `docs/wiki/About.md` for how to run the server and `docs/phase-0-exit-criteria.md` for the current phase gate.
