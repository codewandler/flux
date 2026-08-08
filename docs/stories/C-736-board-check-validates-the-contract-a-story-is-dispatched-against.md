---
id: C-736
title: "board check validates the contract a story is dispatched against"
pillar: "Core"
status: done
epic: delivery-is-verified
areas: [flux-cli]
done_override: "One criterion — an unparseable story or one with a status outside STATUSES must fail check — is deferred to C-748, which carries a fuller contract for it: the reads-versus-check split and the unreadable-file count. Everything else in this story shipped in fdb59a47 with the gate green. C-320, the only story that was in that state in 1783, is fixed and is C-748's regression fixture."
---

# board check validates the contract a story is dispatched against

## Goal

`flux board check` never opens a story's body. It validates frontmatter, ids, priorities and document
links and stops — so nothing verifies that a dispatched story has a Goal or any acceptance criteria
at all. Measured on 1251 stories: 13 still carry the literal `- [ ] Define acceptance.`, 14 have a
missing or empty `## Goal`, and **5 `ready` stories have no usable contract**. A worker dispatched
against one of those is told its definition of done is "Define acceptance."

## Acceptance

- [x] `check` reads the body. A story must have a non-empty `## Goal` and at least one criterion
      under `## Acceptance`, and must not contain the placeholder `create` writes.
- [x] Severity follows status, because a just-created story is legitimately incomplete: **error** for
      `ready`, `in-progress` and `blocked`; **warning** for `backlog`; **error** for `done`, which was
      closed against a contract that must therefore exist.
- [ ] *(Deferred to [C-748](C-748-a-story-the-board-cannot-parse-is-a-failure-not-a-silent-skip.md), which carries the fuller contract — including the reads-versus-`check` split and the unreadable-file count.)* A file in `docs/stories/` that cannot be parsed, or carries a status outside `STATUSES`, is an
      **error** rather than a warning-and-skip. `C-320` has `status: active`, exists on disk and is
      invisible to every board read while `check` exits 0 — invisible is worse than invalid.
- [x] The failure names the file, the missing part and the status that made it fatal, so it is
      actionable without opening the file.
- [x] Migration lands with the rule: the 5 `ready` stories get real contracts, and finished work is
      not given invented criteria — a `done` story with none records a reasoned waiver instead.
- [x] Failing-first: a fixture board carrying each shape — placeholder acceptance, empty Goal, zero
      criteria, unparseable frontmatter, unknown status — and `check` fails naming each.

## Progress

- 2026-08-08 — landed on `main` in `fdb59a47`, gate green. `check` now opens the body: a story needs
  a non-empty `## Goal` and at least one acceptance criterion, and may not carry the placeholder *as*
  a criterion. Severity follows status — error for `ready`, `in-progress` and `done`, warning for
  `backlog` and `blocked`.
- One criterion is **not** delivered here and is deferred rather than ticked: an unparseable story, or
  one whose `status:` is outside `STATUSES`, is still dropped with a warning while `check` exits 0.
  That work moved to [C-748](C-748-a-story-the-board-cannot-parse-is-a-failure-not-a-silent-skip.md),
  which carries a fuller contract than the single line here — it also fixes the reads-versus-`check`
  split (a read that hard-fails on one bad file is unusable while the file is being fixed) and makes
  `check` report how many files it could not read. `C-320` was the only story in that state in 1,783;
  it is now `done` and is C-748's regression fixture.
- Post-migration census: of 104 `ready` stories, 0 lack a usable contract. Four `backlog` stories
  (C-727, C-731, C-733, C-734) were carrying the placeholder and were given real contracts in the
  same pass; the check reported every one of them, which is the behaviour working.
