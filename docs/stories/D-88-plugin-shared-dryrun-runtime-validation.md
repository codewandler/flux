---
id: D-88
title: Shared dry-run/runtime validation layer for plugins (host-kit)
pillar: Core
status: backlog
priority:
epic: gitlab-plugin-hardening
design: docs/designs/gitlab-plugin-hardening.md
note: "keystone — dry-run trusts only the JSON schema while the runtime enforces more, so ~14 gitlab findings are 'green --dry-run → immediate runtime failure'; give host-kit one preflight both paths run (GL-004/008/011/012/020/021/022/024/025/027/028/029/030/036)"
---

# Shared dry-run/runtime validation layer for plugins (host-kit)

## Goal
Close the schema↔runtime validation gap once, in `host-kit`, so an operator or agent gets the same
verdict from `--dry-run` as from a live call. Today `--dry-run` validates only the generated JSON
schema, while runtime handlers enforce more (blank-string trimming via `flex_str`, non-empty arrays,
positive ids, conditional target requirements, enum values, regex compilation, nested payload
types). The drift produces a green dry-run for inputs guaranteed to fail — the single highest-value
fix in the [gitlab-plugin-hardening](../designs/gitlab-plugin-hardening.md) epic.

## Why (evidence)
A beta pass found ~14 gitlab ops where `--dry-run` returns `valid:true` and the runtime then rejects
the same input before HTTP. The plugin's own contract test passes, so this is contracted behavior,
not a build break — it needs an explicit shared preflight, not a patch per op.

## Acceptance
- [ ] `host-kit` gains a preflight validation hook invoked by **both** the `--dry-run` path and the
      runtime dispatch, so the two never disagree; a failing-first test asserts a case that currently
      passes dry-run and fails runtime now fails dry-run too.
- [ ] Unknown input fields are rejected (or clearly warned) rather than silently accepted (GL-008).
- [ ] Enum-like fields validate against their allowed set locally: `issue.list state`,
      `project.create visibility`, `release.link.create link_type`, `ci.variable.create
      variable_type`, and `repository.archive format` (GL-011, GL-022).
- [ ] Non-empty-array constraints enforced in dry-run for `commit.create actions` and
      `snippet.create files` (GL-020); nested payload element types validated (GL-012).
- [ ] Numeric ids/iids reject zero/non-positive (GL-024); blank/whitespace required strings are
      rejected in dry-run to match `flex_str` runtime behavior (GL-030).
- [ ] Conditional target requirements are expressed and enforced — `ref OR (project AND iid)` for
      mr/issue ops, required `snippet_id` for `snippet.delete`, and MR line-anchor conditionals
      (`path` + `new_line`/`old_line`) — via `oneOf`/dependency schema or the preflight hook
      (GL-004, GL-029, GL-036).
- [ ] `mr.diff.lines search` regex is compiled and reported invalid at dry-run time (GL-027).
- [ ] Empty-update guards are consistent across `mr.update`/`issue.update`/`release.update`
      (GL-021).
- [ ] Handler-accepted aliases (`tag`/`name`→`tag_name`, `name`→`branch`, `id`→`snippet_id`) are
      either surfaced in the generated schema or accepted by dry-run (GL-028).
- [ ] The `mr.discussion.create` preview and CLI `--dry-run` are reconciled into one documented
      concept, or the difference is made explicit in the op description (GL-025).
- [ ] `cargo build/test/clippy -D warnings/fmt` green for `host-kit` + `gitlab` and the plugins
      workspace; new MockHost tests per rule.

## Progress
- Not started.

## Notes
- Keystone of the epic — schedule before the gitlab-specific stories; several of their acceptance
  items assume this preflight exists.
- Cross-plugin: the same drift almost certainly exists in the other native plugins, so the fix lives
  in `host-kit`, not `gitlab`. Landing it here is a template for a later sweep.
