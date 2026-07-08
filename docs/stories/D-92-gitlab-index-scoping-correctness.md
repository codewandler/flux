---
id: D-92
title: gitlab — index scoping correctness & scope estimate
pillar: Agent
status: backlog
priority:
epic: gitlab-plugin-hardening
design: docs/designs/gitlab-plugin-hardening.md
note: "index.build {} is a broad instance crawl with no estimate; issue indexing ignores project scope; user/group index inputs are exposed but unimplemented; namespace resolution is 20-page-limited and matches ambiguous basenames (GL-017/026/039/040/046); extends D-38"
---

# gitlab — index scoping correctness & scope estimate

## Goal
Make gitlab indexing and namespace resolution do what their inputs say: honor a project scope for
issues, give a dry-run estimate before a broad crawl, and resolve a namespace unambiguously. Extends
the completed [D-38](D-38-gitlab-parity-ports.md) `index.build` selector work.

## Why (evidence)
A beta pass found `index.build {}` performs a broad instance-wide crawl (all visible projects/MRs/
issues) with no dry-run scope estimate; issue indexing always builds `/issues?scope=all` even when
`project` is supplied (MR indexing already honors `mr_project`/`project`); `index.build` exposes
`user_*`/`group_*` inputs that the implementation never branches on; and `project.create` namespace
resolution searches only the first 20 groups and accepts the first case-insensitive `full_path`-or-
basename match, so a nested group sharing a basename can win.

## Acceptance
- [ ] Issue indexing honors a `project`/`issue_project` scope selector, matching MR indexing (GL-040).
- [ ] `index.build` returns (or its dry-run estimates) the scope it is about to crawl — at least a
      count/among-which signal — so a no-argument call is not a silent broad crawl (GL-017).
- [ ] The unimplemented `user_*`/`group_*` `index.build` inputs are either implemented or removed
      from the schema so the surface stops advertising support that does not exist (GL-039).
- [ ] Namespace resolution in `project.create` paginates beyond the first 20 groups (GL-026) and
      resolves an exact/unambiguous match — a bare basename that matches multiple nested groups is an
      error, not a first-wins pick (GL-046).
- [ ] `cargo build/test/clippy -D warnings/fmt` green for `gitlab`; MockHost tests per changed op.

## Progress
- Not started.

## Notes
- Overlaps D-38's `index.build` selector surface; this is the scoping-correctness follow-on.
