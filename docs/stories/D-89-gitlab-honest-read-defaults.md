---
id: D-89
title: gitlab — honest read defaults, no silent scope-broadening
pillar: Agent
status: backlog
priority:
epic: gitlab-plugin-hardening
design: docs/designs/gitlab-plugin-hardening.md
note: "read/list ops silently broaden or narrow scope: per_page ignored, non-positive limits expand, membership/opened defaults undocumented, project+group→project wins, unknown index selectors succeed as indexed:0 (GL-007/009/010/018/032/033/034/038/041)"
---

# gitlab — honest read defaults, no silent scope-broadening

## Goal
Make the gitlab plugin's read/list defaults explicit and its accepted fields either honored or
rejected — never silently broadening or narrowing the request. A caller should be able to trust that
the input they passed is the query that ran.

## Why (evidence)
A beta pass found several read paths that quietly diverge from the caller's intent: `per_page` is
accepted-but-ignored (only `limit` is read), a `limit` of `0`/`-1` expands to the default set,
`project.list` returns membership-only under "token can see" wording, `search.blobs` takes both
`project` and `group` then silently prefers `project`, and `index.build` with an all-unknown selector
list succeeds with `indexed:0` so a typo reads as an empty success.

## Acceptance
- [ ] `per_page` is either honored (mapped to the GitLab query) or rejected as unknown — not silently
      dropped in favor of `limit` (GL-009).
- [ ] Non-positive `limit`/`max_bytes` mean "none/zero" or are rejected — not "use default / no
      limit" (GL-010); a failing-first test pins the chosen semantics.
- [ ] `project.list`'s membership-only default is documented in the op description (or widened), so
      the wording matches behavior (GL-018).
- [ ] `search.blobs` with both `project` and `group` is rejected as ambiguous, or the precedence is
      documented (GL-032); the same op documents that instance-global blob search needs GitLab
      advanced/exact code search and fails otherwise (GL-007).
- [ ] `job.list scope` non-string entries are rejected in dry-run rather than silently skipped
      (GL-033).
- [ ] `index.build` with a non-empty but all-unknown selector list is a validation error, not
      `indexed:0` (GL-034).
- [ ] `mr.list`/`issue.list` default state is documented (and consistent with `index.build`'s `all`),
      so "list issues" does not silently hide closed/merged records (GL-038).
- [ ] Group-scoped `search.blobs` either honors `ref` or rejects it as unsupported for the group
      scope (GL-041).
- [ ] `cargo build/test/clippy -D warnings/fmt` green for `gitlab`; MockHost tests per changed op.

## Progress
- Not started.

## Notes
- Depends on D-88's shared preflight for the reject-unknown-field and non-string-entry cases.
