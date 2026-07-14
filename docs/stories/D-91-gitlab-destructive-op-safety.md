---
id: D-91
title: gitlab — destructive-op risk metadata, confirm fields & project.delete
pillar: Agent
status: done
priority: 3
epic: gitlab-plugin-hardening
design: docs/designs/gitlab-plugin-hardening.md
note: "all create/update/delete/merge are flat 'Medium' risk with no confirm field; no plugin-native project.delete (lifecycle tests need raw REST); changelog.add writes to the default branch/file with only project+version (GL-001/005/037)"
---

# gitlab — destructive-op risk metadata, confirm fields & project.delete

## Goal
Differentiate destructive gitlab operations from ordinary reversible writes so an agent (and the
approval surface) can treat them with proportionate care, and close the one lifecycle gap that forced
a beta tester out to raw GitLab REST.

## Why (evidence)
A beta pass found every create/update/delete/merge op marked flat `Medium` risk, so
`branch.delete_merged` (bulk) looks no riskier than editing one field; delete ops accept only target
identifiers with no confirmation; `project.create` has no `project.delete` counterpart (cleanup used
raw REST); and `changelog.add` will commit generated content to the default branch and `CHANGELOG.md`
with only `project` + `version`.

## Acceptance
- [ ] Destructive/bulk ops carry finer risk/effect metadata than reversible writes —
      `branch.delete`, `branch.delete_merged` (higher, bulk), `repository.tag.delete`,
      `release.delete`, `ci.variable.delete`, `repository.file.delete` (GL-005).
- [ ] Delete ops gain an optional explicit-confirmation field (e.g. `confirm_branch`,
      `confirm_tag_name`, `confirm_project`) that the op requires before mutating (GL-005).
- [ ] `gitlab.project.delete` exists with clear destructive metadata and a `confirm_path`/
      `confirm_project_id` guard, enabling a plugin-native reversible repo lifecycle (GL-001).
- [ ] `changelog.add` requires a safer explicit target — an explicit `branch` (and/or `file`,
      `message`) — rather than defaulting to the default branch and `CHANGELOG.md` (GL-037).
- [ ] `cargo build/test/clippy -D warnings/fmt` green for `gitlab`; MockHost tests per changed op,
      including a test that the confirm guard blocks an unconfirmed delete.

## Progress
- Not started.

## Notes
- Risk-metadata shape should stay consistent with how the host/approval surface already reads
  op effect metadata; coordinate with the confirm-field convention if one lands cross-plugin.
