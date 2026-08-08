---
id: C-737
title: "A missing or suffixed Acceptance heading cannot pass for a satisfied contract"
pillar: "Core"
status: done
epic: delivery-is-verified
areas: [flux-cli]
---

# A missing or suffixed Acceptance heading cannot pass for a satisfied contract

## Goal

Two parser holes let a story pass as satisfied when nothing checked it.

`board done` refuses when `remaining > 0` — but `remaining == 0` is also true when `total == 0`, so a
story with **no** `## Acceptance` section closes with no override and no complaint. `D-08`, `D-14`,
`D-15`, `D-16` and `D-17` shipped exactly that way.

And `checkbox_counts` matches the heading exactly, so `## Acceptance (for the epic)` and
`## Acceptance — stage 1` report zero criteria. Ten stories have contracts no tool can see, including
`C-418`, `C-419`, `C-420` and `C-599`.

## Acceptance

- [x] `board done` refuses a story with zero acceptance criteria. Absence and satisfaction must not
      produce the same verdict. `--override-reason` remains the recorded escape.
- [x] `checkbox_counts` recognises `## Acceptance` followed by anything, so a suffixed heading is
      read rather than silently reporting zero.
- [x] Every consumer of that parser sees the recovered criteria — `board done`, `board reconcile`'s
      `acceptance-complete`, `board stats`, and C-723's `verify_already_built`.
- [x] Regression test: a fixture story with `## Acceptance (for the epic)` and one unticked box is
      refused by `board done`, and the same story with zero criteria is refused for the other reason.
- [x] The ten affected stories are re-counted after the fix and their criteria are visible.

## Progress

- 2026-08-08 — landed on `main` in `fdb59a47`, gate green.
- `heading_opens_section` accepts a heading that *opens* the section rather than one that equals it,
  so `## Acceptance (for the epic)` and `## Acceptance — stage 1, post-hoc transcript` are read as
  the Acceptance section. It still refuses `## Acceptances`: the character after the heading must be
  non-alphanumeric, so a longer word is not a qualified heading.
- **Measured recovery, same tree, two binaries.** `board stats` before the fix reported
  `criteria.total = 6781`; after, `6834`. **53 acceptance criteria existed on disk that no tool could
  see** — and since `stats` resolves items at a git ref, both runs read identical input, so the
  entire difference is the parser. That is also the evidence for the "every consumer" criterion:
  `stats` shares `checkbox_counts` with `board done`, `board reconcile`'s `acceptance-complete` and
  C-587's reviewer, so recovering it once recovers it everywhere.
- The `board done` hole is closed in the same change: `total == 0` now refuses rather than passing
  as "nothing remaining", with `--override-reason` still the recorded escape. Absence and
  satisfaction are no longer the same value.
