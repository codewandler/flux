---
id: D-38
title: gitlab fluxplane parity ports — list pagination/filter, index.build selector, byte caps, mr.merge drift
pillar: Agent
status: done
priority:
epic:
design:
note: field-by-field re-audit vs ~/projects/fluxplane/fluxplane-plugins/gitlab/ (audit: .flux/plans/d36-parity-audits/gitlab.md) surfaced 7 ops with real feature gaps + 1 handler drift; all ported from the Go reference
---

# gitlab fluxplane parity ports

## Goal

Close the fluxplane parity gaps the D-36 re-audit surfaced in `gitlab` (the schemars migration
locked flux's existing — gapped — contracts; this ports the missing params + handler logic from
the fluxplane Go reference). Audit: `.flux/plans/d36-parity-audits/gitlab.md`.

## Acceptance
- [x] `gitlab.mr.merge` drift fixed: `remove_source_branch` (handler already read it, schema omitted)
      now in `MrMergeInput` + contract.
- [x] List-op pagination/filter parity: `project.list`/`mr.list`/`issue.list`/`pipeline.list` gained
      `limit`/`query`/`order_by`/`sort` (+ per-op filters: `mr.list` `source_branch`/`target_branch`;
      `pipeline.list` `status`/`ref`/`source`/`username`); handlers thread them into the GitLab API
      query (`limit`→`per_page`). Contracts updated.
- [x] `gitlab.index.build` selector surface: `index`/`indexes`/`entity`/`entities` selectors +
      per-datasource tuning (`search`/`query`/`order_by`/`sort`/`membership`); a caller can now ask
      for just `projects` (or `issues`/`merge_requests`) instead of always indexing all three.
- [x] `gitlab.repository.file.show` `max_bytes` — content truncated on a char boundary, `truncated`
      flag set.
- [x] `gitlab.search.blobs` `max_data_bytes` — per-match snippet cap (char-boundary) +
      `data_truncated` flag.
- [x] Failing-first MockHost tests per change; `cargo build/test/clippy -D warnings/fmt` green
      for `gitlab` (44 tests, +9).
- [x] `endpoint_ref` architectural split left as-is (do-not-port).

## Notes
- The ~58 shared-alias cases (handler accepts `project_id`/`path`/`id`/`name` aliases the schema
  omits — intentional leniency) are out of scope.
- Audit report: `.flux/plans/d36-parity-audits/gitlab.md`.
