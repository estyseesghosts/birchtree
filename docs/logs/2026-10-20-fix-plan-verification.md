# 2026-10-20 fix-plan verification

Gate per `docs/fix-plan.md` step 5: `cargo check --all-targets`
warning-free + `cargo test --release` green + HTTP smoke for behavior
changes. Commits: `372f216` (P0/P1), `f1cda48` (P2), `163ee81` (docs).

## `cargo check --all-targets`: clean, 0 warnings
(dev profile; final run after all merges).

## `cargo test --release`: EXIT 0, 60 `test result: ok`, 0 failed
(`/tmp/opencode/release-test.log`; full output retained out of tree).

## `cargo test` (debug): 60 suites ok, 0 failed
(one interim failure `context_ancestors_descendants` was P2a's in-flight
ordering change — asserted pre-promotion order; fixed to post-promotion
`[deep, reply]` with the upstream `threading_concern` cite; one interim
failure `vote_journals_cast_ballots_only` encoded the pre-fix application
behavior — empty ballot now `Ok` at the use-case layer per
`VoteService#call:22`, 400 preserved at the endpoint for a missing key).

## Live HTTP smoke (debug binary, fresh data dir, `--no-transcode`)
- `GET /api/v1/instance` → 200
- `GET /api/v1/instance/peers` → 200 `[]` (no remotes yet)
- `GET /api/v1/instance/rules` → 200 `[]`
- `GET /api/v1/instance/activity` → 200, 12 `{week,statuses,logins,
  registrations}` string windows
- `GET /api/v1/instance/languages` → 200
- `GET /api/v1/instance/domain_blocks` → 404 (default hidden, upstream)
- `GET /api/v1/instance/terms_of_service` → 404 (no text setting yet)
- `GET /api/v1/directory?limit=3` → 200 (anonymous)
- `GET /api/v1/suggestions` (anon) → 401; `DELETE /api/v2/suggestions`
  → 405 (recorded: upstream 404s, Axum 405s on the GET path)
- `GET /.well-known/webfinger?resource=acct:nobody@localhost` → 404
- `GET /nodeinfo/2.0` → 200 with `metadata.nodeName/nodeDescription`
- `GET /api/v1/timelines/public`, `/tag/cats` → 200 `[]`
- `GET /api/v1/donation_campaigns` (anon) → 401 (user gate before the
  honest-absence 404)
