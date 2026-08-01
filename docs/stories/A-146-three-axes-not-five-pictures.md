---
id: A-146
title: "Give the mocks the three controls — depth limit, condense-completed, optional detail pane"
pillar: Agent
status: ready
priority: 5
design: docs/designs/agent-loop-visibility.md
epic: agent-loop-visibility
areas: [flux-tui]
note: "⚠ owner-directed after driving A-144's mocks: the five are not five candidates, they are points in a space with three ORTHOGONAL axes. The flat thread with condensing + a depth limit + an optional pane IS the split. Prove that in the mocks before A-137 commits to it"
---

# Turn the five pictures into three knobs

## Goal

Add depth limit, condense-completed and an optional detail pane to A-144's mock harness — and show
that composing them reproduces the five layouts, so A-137 can build **axes** instead of adopting a
picture.

## The claim to test

| axis | what it does | which mock it becomes |
|---|---|---|
| **depth limit** | how many nesting levels are drawn | flat thread ↔ tree |
| **condense completed** | finished work collapses to one row | the long-run win |
| **detail pane** | optional, toggled | tree ↔ split |

If that holds, the mocks stop being rivals and A-137's decision changes from *"which layout"* to
*"which defaults"*.

⚠ **This converges with A-144's review from the other direction.** That comparison was confounded
because only mock 3 had phase-condensing, forcing the recommendation to *"condense finished phases
first, then build the split"* — separability was the finding. This story says all three are separable
and makes separation the design.

## Acceptance

- [ ] **Failing-first**: a test asserting that the flat thread with condensing on, a depth limit and the
      pane enabled renders equivalently to the split — failing at the merge base.
- [ ] The three controls are real and independent — each settable without the others, driveable from the
      example's keys, and covered across A-144's existing viewport envelope (18 widths × 28 heights) and
      four load cases.
- [ ] ⚠ **Elision stays visible at every setting.** A-144's rework made this unconditional via
      `Tally::finish`; three new ways to hide things must not create a way around it. A depth limit that
      hides a level, or condensing that collapses a phase, is *withholding* and must say so.
- [ ] ⚠ **Condensing must not swallow a failure.** A finished step that *errored* or was retried is
      exactly what a reader needs to see; collapsing it into a tidy one-line summary would make the view
      flatter than the run. Pin it with a case containing an error.
- [ ] ⚠ **A depth limit must not silently hide a sub-agent's work.** Say how many levels are withheld,
      not just that some are.
- [ ] The floors are re-measured per configuration. The split's 64-column / 10-row floor was its main
      cost; with the pane optional, that floor should apply only when it is on — confirm rather than
      assume.
- [ ] The recommendation is updated to name **defaults** rather than a layout, and says what each default
      shows and hides.
- [ ] Full gate green.

## Notes

- Builds on A-144's harness and on [A-145](A-145-a-real-run-as-the-mock-fixture.md)'s real-run fixture;
  ⚠ **branch after A-145 lands** — both work in `crates/flux-tui/src/loopmock/` and would collide.
- ⚠ Defaults are the honesty question. Every axis should default to showing *more* than it hides; a view
  that flatters the run is the risk this epic has carried from the start.
- If composing the axes turns out **not** to reproduce the five — if the split's rail is doing something
  a depth-limited thread cannot — that is the most valuable finding available and it belongs in the
  recommendation, not in a workaround.

## Progress

- Filed 2026-08-02 from the owner's read of the rendered mocks.
