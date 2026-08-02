---
id: A-146
title: "Give the mocks the three controls — depth limit, condense-completed, optional detail pane"
pillar: Agent
status: in-progress
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

- [x] **Failing-first**: a test asserting that the flat thread with condensing on, a depth limit and the
      pane enabled renders equivalently to the split — failing at the merge base.
      → ⚠ **satisfied as a refutation.** `the_axes_reach_the_split_only_with_one_root_or_too_few_rows`
      is that test, swept over the whole axis space rather than the single setting named, and it pins
      the measured answer: they are **not** equivalent. See "The claim" below.
- [x] The three controls are real and independent — each settable without the others, driveable from the
      example's keys, and covered across A-144's existing viewport envelope (18 widths × 28 heights) and
      four load cases.
      → `each_axis_moves_the_drawing_on_its_own`; `the_whole_axis_space_stays_inside_its_viewport_…`
      sweeps 12 configurations × 4 cases × 18 × 28; keys `x [ ] c p 0` in `examples/loop_mocks.rs`.
- [x] ⚠ **Elision stays visible at every setting.** A-144's rework made this unconditional via
      `Tally::finish`; three new ways to hide things must not create a way around it. A depth limit that
      hides a level, or condensing that collapses a phase, is *withholding* and must say so.
      → both new axes report through `Tally`; each note is built with `shorten` and the **drawn**
      string is registered as the marker, so the honesty net never has to fire on an honest render.
- [x] ⚠ **Condensing must not swallow a failure.** A finished step that *errored* or was retried is
      exactly what a reader needs to see; collapsing it into a tidy one-line summary would make the view
      flatter than the run. Pin it with a case containing an error.
      → `condensing_never_swallows_the_recorded_failure`, against the real failed `git_stage`.
- [x] ⚠ **A depth limit must not silently hide a sub-agent's work.** Say how many levels are withheld,
      not just that some are. → `a_depth_limit_reports_the_number_of_levels_it_withheld`; the marker
      also names how many of the withheld steps failed.
- [x] The floors are re-measured per configuration. The split's 64-column / 10-row floor was its main
      cost; with the pane optional, that floor should apply only when it is on — confirm rather than
      assume. → `the_panes_floor_travels_with_the_pane_and_not_with_the_layout`. **Confirmed:** pane
      off draws every case at 40×6; pane on refuses at 63×10 and draws at 64×10.
- [x] The recommendation is updated to name **defaults** rather than a layout, and says what each default
      shows and hides. → `loopmock::RECOMMENDATION` section 6.
- [x] Full gate green.

## The claim, and what the measurement said

**Does the composition hold? No — and the refutation is the deliverable.**

| claim | verdict |
|---|---|
| the tree is a point in the space | ✅ exactly — `depth 6 · condense off · pane off`, every case |
| the flat thread's *view* is a point | ✅ exactly — `depth ∞ · condense off · pane off` |
| depth limit ⇒ flat thread ↔ tree | ❌ **mocks 1 and 2 are the same point.** Neither hides anything the other shows; what separates them is **indentation**, which no show/hide axis can express |
| pane ⇒ tree ↔ split | ❌ **the split is not in the space.** The axes reach it only with one top-level step, or too few rows for its rail |

The structural reason: mock 3's rail is *one row per top-level step plus the focused top-level step's
entire subtree, completed work included* — it discriminates on **focus**; condensing discriminates on
**status**. With one root they coincide (60 of 60 viewports); with nine turns they cannot (0 of 24).

⚠ **A-137 owes a fourth decision, not a fourth picture: condensing's GRANULARITY.** "Finished work
collapses to one row" does not say at what level, and a one-bit toggle cannot express the difference
between folding uniformly and folding only at the top level (which is mock 3's rail).

⚠ **The honesty worry inverted.** On the real run, condensing is what **buys the room** to show a
failure. Turn 7's `git_stage` failed `exit 128` inside an `execute_batch` that closed `ok`; uncondensed
it is 166 steps back and off screen, condensed it is visible because *turn 7 refuses to fold*.

**Defaults: `depth All · condense on · pane off`.** Reasoning and what each shows/hides in
`RECOMMENDATION` section 6.

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
- Implemented 2026-08-02 in `crates/flux-tui/src/loopmock/axes.rs`: `Depth`, `Axes`, `AXIS_SPACE`,
  `Shape`, `render_axes`. Twelve new tests in `crates/flux-tui/tests/loop_mocks.rs`; the composed
  view's evidence is the new A-146 section of `docs/designs/agent-loop-visibility-mocks.md`.
- ⚠ `Render.steps_drawn: usize` became `Render.represented: BTreeSet<usize>` (+ a `steps_drawn()`
  accessor), and `Tally::drew` now takes ids. **This was load-bearing, not tidying:** A-145 measured
  the split and the flat thread hiding the same *number* of steps (166) and a different *set*, so a
  comparison done on counts would have called them equivalent and the whole story would have
  concluded the opposite of what it did.
- ⚠ **A `Condense::TopLevel` mode was deliberately NOT added.** It would have made the composition
  close by encoding mock 3's rail as a third setting of the condense axis — manufacturing the
  confirmation the story warned against. The missing parameter is named in the recommendation
  instead, so A-137 decides it.
- ⚠ An earlier reading of the same measurement, taken only at 100×28, concluded the axes agree with
  the split "exactly when the split is hiding nothing". The full envelope sweep falsified it (at
  64×10 the split withholds 10 of 18 and two configurations still match); the corrected boundary is
  **one root, or too few rows**. Both the test's doc comment and `RECOMMENDATION` section 2 record
  the correction rather than only the conclusion.
- `docs/designs/agent-loop-visibility.md`'s "three axes" section rewritten from an assertion into a
  correction, naming the refuted claim as the owner's own.
