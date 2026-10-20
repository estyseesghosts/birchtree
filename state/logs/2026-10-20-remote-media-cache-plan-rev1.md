# 2026-10-20 — revise remote-media cache plan from review

Two parallel review subagents returned verdicts; all load-bearing claims
spot-verified in-tree before revising (no review point applied on trust).

## Review outcome (birchtree)

- Blocker confirmed: §1 "still stubs" ignored the working eager path
  (`src/federation/media.rs:24-102`, `REMOTE_MEDIA_SOURCE` +
  `stage_remote_media` in
  `src/persistence/redb/federation_content.rs:381-441`, ingest enqueue in
  `src/application/federation_inbound.rs:178-183`, `domain/job.rs:53`,
  `jobs/worker.rs:71`). Fixed: §1 now three-bullet state; plan reframed as
  conversion + split.
- Majors applied: build order reordered (split → trigger conversion →
  reconciler → warming); dedup frozen for v1 (per-row keys retained,
  shared-blob test N/A); `remote/*` migration named; 40 MiB per-file cap
  precedence stated (`media.rs:20` stays as ceiling); §8 downgraded to
  sequencing note; Global 512 MiB table row; avatar gap, 404-admin-until-7.3
  observability routing, unsigned media GET gap, split `net.rs`/`media.rs`
  citations, proxy/relay hedges marked.

## Changed

- Rewrote `docs/remote-media-cache-plan.md` (Status stays Draft) per above.

## Verified

- Cited code re-read during revision (`media.rs:20,61-90`,
  `federation_inbound.rs:126-183`, `net.rs:58-95`, `federation-contract.md:36-40`,
  `birchtree.md` §§7/9 Phase 7.3 admin-404). Docs-only session; no test run.

## Open

- Normative deltas still to mirror into `federation-contract.md` /
  `persistence-conformance.md` when Phase 6 begins.
