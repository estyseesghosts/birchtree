# HANDOFF — birchtree avatar/header upload

## Why
`PATCH /api/v1/accounts/update_credentials` accepts urlencoded
display_name/note only. Avatar/header uploads are called out in the code
as a named follow-up (`src/api/mastodon/endpoints/accounts.rs`: "`avatar`/
`header` need the media pipeline"). Every seeded rig account therefore has
the default `missing.png`, and Keystone profile editing can't set photos.
Mastodon 4.7.3 clients (and Keystone) upload avatars/headers as multipart
form fields on this endpoint — implement that.

## What to implement (pinned to Mastodon v4.7.3, `birchtree.md` source of truth)
1. **Multipart parsing on update_credentials** — accept `avatar` and
   `header` file parts alongside the existing urlencoded fields. Keep
   accepting urlencoded (current rig/clients depend on it); multipart and
   urlencoded must not regress each other.
2. **Media pipeline** — run uploads through the existing media/transcode
   path: content-type allowlist (same image types the startup gate probes),
   size caps vs `max_file_size`, staging under the blob store, then persist
   references on the account row in the same `update_account_profile`
   transaction style as the rest.
3. **Federation** — updated `icon`/`image` in served actor documents and
   `update`s fanned out, so remote servers (and the rig's pinetree nodes,
   which render `avatarUrl` from these) pick up the change. Check
   refetch/update propagation, not just local reads.
4. **Deletes/resets** — Mastodon semantics for clearing (empty value)
   if applicable; invalid uploads → 422 validation error in the file's
   existing style, not 415/500.

## Definition of done
- `curl` repro: create app → PKCE user token → multipart PATCH with avatar
  + header → `verify_credentials` returns the new URLs; restart-safe.
- Remote visibility: a pinetree `ap/show` on the actor shows the new
  `avatarUrl` after refetch/update.
- `cargo check --all-targets`, `cargo test`, and the phase gates green;
  new tests cover multipart + urlencoded, oversize/rejected types, and the
  actor-document fields. Record in `docs/` + dated `state/logs/` per AGENTS.md.

## Context
- Rig: `/root/rig` (bt-a `http://127.10.0.21:8080`, u0..u7/password123);
  nodes run `--no-transcode`, but the `birchtree:latest` image has the full
  ffmpeg toolchain if the pipeline needs it.
- Seeder (`seed.py cmd_profiles`) currently sends display_name/note only;
  once this lands, extend it to upload the per-user avatars/banners in
  `/root/rig/assets/` so federated avatars are covered by the testbench.
- Keystone (akkoma/mastodon flavor) profile editor is the client-side
  acceptance path.
