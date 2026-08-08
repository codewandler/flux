---
id: C-736
title: "board check validates the contract a story is dispatched against"
pillar: "Core"
status: backlog
priority: 1
epic: delivery-is-verified
areas: [flux-cli]
---

# board check validates the contract a story is dispatched against

## Goal

`flux board check` never opens a story's body. It validates frontmatter, ids, priorities and document
links and stops — so nothing verifies that a dispatched story has a Goal or any acceptance criteria
at all. Measured on 1251 stories: 13 still carry the literal `- [ ] Define acceptance.`, 14 have a
missing or empty `## Goal`, and **5 `ready` stories have no usable contract**. A worker dispatched
against one of those is told its definition of done is "Define acceptance."

## Acceptance

- [ ] `check` reads the body. A story must have a non-empty `## Goal` and at least one criterion
      under `## Acceptance`, and must not contain the placeholder `create` writes.
- [ ] Severity follows status, because a just-created story is legitimately incomplete: **error** for
      `ready`, `in-progress` and `blocked`; **warning** for `backlog`; **error** for `done`, which was
      closed against a contract that must therefore exist.
- [ ] A file in `docs/stories/` that cannot be parsed, or carries a status outside `STATUSES`, is an
      **error** rather than a warning-and-skip. `C-320` has `status: active`, exists on disk and is
      invisible to every board read while `check` exits 0 — invisible is worse than invalid.
- [ ] The failure names the file, the missing part and the status that made it fatal, so it is
      actionable without opening the file.
- [ ] Migration lands with the rule: the 5 `ready` stories get real contracts, and finished work is
      not given invented criteria — a `done` story with none records a reasoned waiver instead.
- [ ] Failing-first: a fixture board carrying each shape — placeholder acceptance, empty Goal, zero
      criteria, unparseable frontmatter, unknown status — and `check` fails naming each.
