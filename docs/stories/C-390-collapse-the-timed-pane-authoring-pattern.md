---
id: C-390
title: Collapse the timed-pane authoring pattern in the docs and the shipped demo flow
pillar: Language
status: backlog
epic: agent-change-recovery-and-provenance
design: docs/designs/agent-change-recovery-and-provenance.md
note: "LANG-01 does NOT validate as a language gap — the collapsed `each` over a frame table with a nested `loop for …, every:` parses, lowers and RUNS correctly today (executed during validation). The verbosity is one hand-unrolled demo flow and a missing docs row"
---

# Collapse the timed-pane authoring pattern

## Goal

Fix the example that teaches the verbose form, rather than adding language surface for a limitation
that does not exist.

## Acceptance

- [ ] `website/docs/language/control-flow.md`'s pattern table gains a "paced sequence over a list"
      row showing `each … / loop for …, every: …`.
- [ ] `.flux/flows/pane_animation_demo.flux` is rewritten from eight hand-unrolled one-iteration
      loops to the collapsed frame-table form.
- [ ] A runtime test asserts the collapsed form dispatches exactly one update per frame, in list
      order, with the item variable bound inside the nested loop.
- [ ] No new operation and no language change: H's review explicitly says the exercise did not
      justify a `pane.sequence` op, and validation found no gap that would.

## Progress

- 2026-08-01 — the collapsed form was authored and executed during validation: 3 dispatches, correct
  order, item bound through the nested loop.

## Notes

- Cancellation and approval behaviour already work through the existing loop constructs; this is
  documentation and one example file.
