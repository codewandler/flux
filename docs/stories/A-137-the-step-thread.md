---
id: A-137
title: "The loop view becomes the TUI's main display, with view options — depth, condensing, detail, variables"
pillar: Agent
status: ready
priority: 5
design: docs/designs/agent-loop-visibility.md
epic: agent-loop-visibility
areas: [flux-tui]
note: "⚠ SCOPE CHANGED 2026-08-02, owner-directed: no longer a thread beside the transcript — the TUI's PRIMARY surface. Two things to decide before coding: where the conversation goes, and that `show variables` puts run state on the DEFAULT screen, escalating A-142's disclosure requirement from an opt-in pane to what is on screen during every demo"
---

# The main display is the program running

## Goal

Make the loop view the TUI's primary surface, with view options controlling how much of it is shown:
nesting depth, condensing of completed work, an optional detail pane, and optionally the variables a run
is holding.

## What changed

This was filed as *"the step thread"* — one line per finished step, **beside** the existing transcript.
The owner has since driven [A-144](A-144-five-tui-mocks-of-one-flow.md)'s rendered mocks and asked for
something larger: **the loop view as the main display**, with the axes as user-facing view options.

The axes are settled ([A-146](A-146-three-axes-not-five-pictures.md) proves the composition in the mocks
first): **depth limit · condense completed · optional detail pane**. This story adds the fourth the
owner named — **show variables** — and makes the whole thing primary rather than adjacent.

## ⚠ Two things to decide before writing code

**1. Where does the conversation go?** `flux tui` is documented today as *"the ratatui chat TUI"*.
People type a message and read prose back. If the loop view is the main display, the assistant's actual
*answer* needs a home — and "it scrolls past in a pane" is a downgrade of the thing most users came for.
⚠ This is a product decision about what the TUI **is**: chat-with-an-agent, or watch-a-program-run with
a conversation in it. Decide it explicitly; do not let the layout decide it by accident.

**2. ⚠ `show variables` puts run state on the default screen, and that escalates a safety requirement.**
[A-142](A-142-inspect-a-paused-run.md) treats inspection as a *debugger* surface — opt-in, deliberately
entered — and already requires everything shown to route through the `Redactor` **with the failure path
tested**, because [C-339](C-339-redaction-falls-back-to-the-unredacted-value.md) found redaction here
failing *open*. As a view option on the main display it stops being a pane someone chooses to open and
becomes **what is on screen during every demo and screenshare**. The requirement does not change; the
cost of getting it wrong does. Reuse A-142's path — do not grow a second one.

## ⚠ What the real run corrected, and it reshapes this story

[A-145](A-145-a-real-run-as-the-mock-fixture.md) drove the mocks from a real 191-step session instead of
a hand-authored flow. It **confirmed** the headline (condense first, then the split) and corrected three
things that change what to build:

- **Condensing's win is concentrated, not uniform.** Of 55 real phases, **36 are exactly one step** —
  where condensing saves nothing — while **one is 57 steps**, where the whole win lives. The
  hand-authored fixture's even 3–6-step phases made the saving look smooth. ⚠ A condensing design tuned
  for the even case will underperform on a real run; the payoff is bursty.
- **The split and the flat thread hide the *same* 166 of 191 steps.** The split's advantage is
  **coverage** (all nine turns plus the current one in full), not capacity. *"The only one that still
  shows the whole run"* was an artifact of a fixture that was one run rather than nine turns.
- **Real nesting is three levels, not eight.** The indentation tax that cost the tree its ranking was
  charged against a depth this log has never recorded. At 100 columns the tree is the most legible and
  still shows each op's *argument*, which the split's rail cannot.

## Acceptance

- [ ] **Failing-first**: a test asserting the loop view is what the TUI renders by default, and that each
      view option changes what is drawn — failing at the merge base.
- [ ] The four view options are real, independent and discoverable: **depth limit · condense completed ·
      detail pane · show variables**. Built on the axes A-146 validates, not re-derived.
- [ ] ⚠ **Defaults are stated and justified** — with the axes composable, the defaults *are* the design.
      Every axis defaults to showing more than it hides. ⚠ Set the depth default against **three** real
      levels, not the fixture's eight.
- [ ] ⚠ **Elision stays visible at every setting.** A-144's rework made this unconditional via
      `Tally::finish`; four ways to hide things must not create a way around it. A depth limit says how
      many levels are withheld; condensing says what it collapsed.
- [ ] ⚠ **Condensing never swallows a failure.** A completed step that errored or was retried is exactly
      what a reader needs. The A-145 capture contains a real failed `git_stage` — use it.
- [ ] ⚠ **Variables route through the `Redactor`, and the failure path is tested** — not just the happy
      one. Reuse A-142's path.
- [ ] The conversation's home is decided, implemented, and written down.
- [ ] Timing and usage come from what is already carried (`ToolTiming`, `ModelCallTiming`, `CallUsage`) —
      do not re-measure in the view, or the number on screen will disagree with the evidence log.
- [ ] A user who wants today's behaviour can still get it. ⚠ Changing the main display of the daily
      driver needs an escape hatch, at least for one release.
- [ ] Full gate green.

## Notes

- ⚠ **Sequencing**: A-146 proves the composition in the mocks first. Building this before it means
  committing the main display to a shape validated only by argument.
- ⚠ **`crates/flux-tui/src/lib.rs` is 410 KB in one file**, `mod tests` starting around line 4803 — the
  largest module in the workspace, and the reason A-144's mocks live in their own directory. Making the
  loop view primary means working in it. Splitting it is not this story's job; budget the friction or
  file it first.
- ⚠ **A real session has no present tense.** A-145 found the log carries no `Running` status — a replay
  must choose and disclose a cursor. Live, this story *has* the present tense the log lacks, which is
  the one place the live view is strictly better than any replay of it.
- The loop vocabulary is already on the wire: `UiEvent` carries `Planning`, `Plan`, `Phase`, `Intent`,
  `ToolCall`, `ToolTiming`, `ToolResult`, `ToolProgress`, `CallUsage`, `SpawnActivity`.
- ⚠ [A-138](A-138-expand-a-step-into-its-graph.md) may need re-scoping before it is worth building — see
  its own note. A-145 found the captured session accepted **127 plans, every one a single op**, and that
  `plan_ast` is never persisted, so there was no DAG to draw.
- [A-140](A-140-pause-a-live-run.md)'s pause wants a home on a step here, and A-142's inspection is the
  `show variables` option. Leave the seams; build neither.
- Sub-agent activity has a home in `fleet.rs`'s `FleetProjection` — decide whether the view shows it or
  defers, and do not duplicate the projection.

## Progress

- Filed 2026-08-01 with the agent-loop-visibility epic.
- 2026-08-02 — scope changed to "the TUI's main display with view options", owner-directed after driving
  A-144's mocks; A-145's real-run corrections folded in.
- 2026-08-07 — **the three decisions taken, owner-directed**, and the axis work they imply landed:

  1. **The conversation gets a permanent split** beside the loop view — both always visible, no mode.
     ⚠ Still owed: A-146 measured the pane's floor at **64×10**, so the layout must state a fallback
     for narrower terminals rather than draw a broken one.
  2. **Condensing is three-valued** — `Off | Uniform | TopLevel` — which is A-146 section 2's fourth
     decision. `Uniform` is the default; top-level withholds every non-focused turn's interior.
  3. **No indentation axis.** The tree is the picture.

  ⚠ **Taking decision 2 partly falsified A-146's headline, and the measurement is now in the tests.**
  `TopLevel` *is* the split's rail rule, so the axes reach the split where a one-bit flag never could —
  on **fan-out**, 36 of 42 eligible viewports. On the **recorded** nine-turn session, still 0 of 24.
  The salient difference is that run's one real `git_stage` failure: `condensable` refuses to fold a
  subtree holding a failure, so the root the rail would collapse stays open. ⚠ That mechanism is the
  leading candidate, **not** a measured cause — `top_level_condensing_reaches_the_split_only_on_a_run_without_a_failure`
  pins the numbers and says so. On real load, "condensing never swallows a failure" and the rail are in
  tension, and the acceptance wins.

  Landed: `Condense` with `cycle()`, `AXIS_SPACE` swept at the new setting, `render_axes_of` so the live
  view can be the *same* renderer pointed at a live tree rather than a lookalike, A-146's claim rescoped
  rather than deleted, recommendation section 2 rewritten, snapshots regenerated.

  **Still to build**, in order: the live `UiEvent → Step` projection; the permanent-split layout with its
  64×10 fallback; `show variables` through A-142's `Redactor` with the failure path tested; the escape
  hatch to today's display. No acceptance box is ticked yet — none of them is finished.
