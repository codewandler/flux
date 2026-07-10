---
id: D-89
title: gitlab — honest read defaults, no silent scope-broadening
pillar: Agent
status: done
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
- [x] `per_page` is either honored (mapped to the GitLab query) or rejected as unknown — not silently
      dropped in favor of `limit` (GL-009). → Honored: documented alias field on all 15 paginating
      ops; `limit` wins when both are set.
- [x] Non-positive `limit`/`max_bytes` mean "none/zero" or are rejected — not "use default / no
      limit" (GL-010); a failing-first test pins the chosen semantics. → Rejected via
      `range(min = 1)` on the whole limit family, enforced by the D-88 preflight in dry-run AND
      dispatch; `preflight_rejects_non_positive_limits` pins it.
- [x] `project.list`'s membership-only default is documented in the op description (or widened), so
      the wording matches behavior (GL-018).
- [x] `search.blobs` with both `project` and `group` is rejected as ambiguous, or the precedence is
      documented (GL-032); the same op documents that instance-global blob search needs GitLab
      advanced/exact code search and fails otherwise (GL-007).
- [x] `job.list scope` non-string entries are rejected in dry-run rather than silently skipped
      (GL-033). → Typed `Vec<JobScope>` enum: non-strings AND unknown statuses reject.
- [x] `index.build` with a non-empty but all-unknown selector list is a validation error, not
      `indexed:0` (GL-034). → Any unknown selector rejects (mixed lists with a typo too — partial
      silent under-indexing is the same trap); `index_include` returns the error shared by the
      preflight rule and the handler.
- [x] `mr.list`/`issue.list` default state is documented (and consistent with `index.build`'s `all`),
      so "list issues" does not silently hide closed/merged records (GL-038). → Descriptions state
      the `opened` default + that index.build indexes all states; mr.list state is now a typed
      enum (opened|closed|locked|merged|all).
- [x] Group-scoped `search.blobs` either honors `ref` or rejects it as unsupported for the group
      scope (GL-041). → Rejected (GitLab group search has no ref parameter).
- [x] `cargo build/test/clippy -D warnings/fmt` green for `gitlab`; MockHost tests per changed op.

## Progress
- [x] 2026-07-10: implemented on top of D-88's shared preflight — `per_page` alias + `range(min=1)`
      across the limit family, `MrStateFilter`/`JobScope` enums, `pf_search_blobs`/`pf_index_build`
      rules, `index_include` returns Result, honest op descriptions. 5 new tests (66 total green);
      verified end-to-end via `flux plugin call --dry-run` (limit 0, selector typo, ambiguous
      scope, per_page accepted). Plugins workspace gate green; root untouched except
      CHANGELOG/WHATS-NEW (+ website mirror regen).

## Notes
- Depends on D-88's shared preflight for the reject-unknown-field and non-string-entry cases.
- `mr.diff.lines`'s `limit` is a line cap, not pagination — it deliberately did NOT get the
  `per_page` alias (still bounded `range(min = 1)`).
