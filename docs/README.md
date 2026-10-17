# docs/ — contract documents

Each contract file carries a `Status:` line at the top: `Draft` or `Frozen`.
`Frozen` means the Phase 0 exit review accepted it; frozen files change only via a logged superseding decision.

## Index

| File | Status | Plan section |
|---|---|---|
| `phase-0-exit-criteria.md` | Tracking checklist | §9 Phase 0 |
| `phase-1-exit-criteria.md` | Tracking checklist | §9 Phase 1 |
| `phase-2-exit-criteria.md` | Tracking checklist | §9 Phase 2 |
| `phase-3-exit-criteria.md` | Tracking checklist | §9 Phase 3 |
| `phase-4-exit-criteria.md` | Tracking checklist | §9 Phase 4 |
| `phase-4-1-exit-criteria.md` | Tracking checklist | §7 Phase 4.1, §8 item 11 |
| `architecture.md` | Draft | §3 |
| `source-audit-v4.7.3.md` | Draft | §2 |
| `compatibility-profile.md` | Draft | §1, §6 |
| `endpoint-manifest.md` | Draft | §6 |
| `domain-contracts.md` | Draft | §4 |
| `visibility-and-authorization.md` | Draft | §4 |
| `application-ports.md` | Draft | §3, §4 |
| `persistence-conformance.md` | Draft | §5 |
| `federation-contract.md` | Draft | §2.7, §6 (delegates ActivityPub to `activitypub-federation-rust`) |
| `streaming-contract.md` | Draft | §2.6, §6 |
| `testing.md` | Draft | §8 |
| `logs/` | append-only verification evidence | §3 |

Source-audit authority: Mastodon v4.7.3 (`~/fedifedifedi/mastodon`, commit `8ff11c2b`); route inventory from `config/routes/api.rb`; behavior explanations from controllers/services/policies/serializers/workers as cited in `birchtree.md`.

Conventions: concrete over abstract (methods, fields, nullability, error shapes); record unsupported behavior explicitly; no implementation code in contracts.
