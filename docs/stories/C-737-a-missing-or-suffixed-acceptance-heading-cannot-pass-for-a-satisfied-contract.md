---
id: C-737
title: "A missing or suffixed Acceptance heading cannot pass for a satisfied contract"
pillar: "Core"
status: backlog
priority: 1
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

- [ ] `board done` refuses a story with zero acceptance criteria. Absence and satisfaction must not
      produce the same verdict. `--override-reason` remains the recorded escape.
- [ ] `checkbox_counts` recognises `## Acceptance` followed by anything, so a suffixed heading is
      read rather than silently reporting zero.
- [ ] Every consumer of that parser sees the recovered criteria — `board done`, `board reconcile`'s
      `acceptance-complete`, `board stats`, and C-723's `verify_already_built`.
- [ ] Regression test: a fixture story with `## Acceptance (for the epic)` and one unticked box is
      refused by `board done`, and the same story with zero criteria is refused for the other reason.
- [ ] The ten affected stories are re-counted after the fix and their criteria are visible.
