---
id: A-144
title: "Five hard-coded TUI mocks of the same flow — pick the loop view by looking at it, not by arguing about it"
pillar: Agent
status: in-progress
priority: 6
design: docs/designs/agent-loop-visibility.md
epic: agent-loop-visibility
areas: [flux-tui]
note: "de-risks A-137 before a line of live plumbing is written. Five DIFFERENT layout strategies, one shared hard-coded flow fixture, rendered side by side. ⚠ The deliverable is a RECOMMENDATION, not five pictures — and a mock that only renders a tidy 6-step flow proves nothing about the run that matters"
---

# Look at five, then choose one

## Goal

Five hard-coded TUI mocks, each rendering **the same flow** with a genuinely different layout
strategy, runnable side by side — so the loop view's shape is chosen by looking at it rather than by
arguing about it in a design doc.

## Why this comes before A-137

[A-137](A-137-the-step-thread.md) has to decide a lot of layout at once: how a finished step condenses,
what "expanded" means, whether the thread is a pane or inline, how nesting reads, what happens to
timing and usage. Those decisions are cheap to change in a mock and expensive to change once live
event plumbing hangs off them.

Everything here is **hard-coded**. No engine, no events, no `UiEvent` wiring — a static fixture and
five renderers. That is the point: it should take a fraction of A-137's effort and it removes most of
A-137's risk.

## The fixture

One shared flow, hand-authored, used by all five renderers so a comparison means something. Use the
**tracking-plugin flow** — validate frontmatter, regenerate the board, audit epics for missing
trackers, sync the CHANGELOG, with a model-authored step in the middle for judgement calls. It is a
flow the team already understands, and it is the same one [C-425](C-425-the-flagship-recipe-tracking-as-a-flux-app.md)
proposes as the flagship recipe, so the work compounds.

⚠ The fixture is **hand-written data**, not a real captured run. That is fine here and it is also the
main thing to be honest about: see the load cases below.

## Acceptance

- [x] Five renderers, each rendering the shared fixture, runnable side by side for comparison — a
      binary, an example, or a snapshot set; whatever makes them easy to *look at* in one sitting.
- [x] ⚠ **Five genuinely different strategies, not five variations of one.** Five ways to draw the same
      thread is a wasted story. Span the space — for example: a flat one-line-per-step thread; a nested
      tree; a thread plus a separate detail pane; a timeline/gantt where duration is the visual;
      graph-first, where the DAG is the primary object and steps annotate it. Take those as a starting
      set, not a specification — if a better fifth exists, use it and say why.
- [x] Each mock carries a short note: **what it optimizes for and what it gives up.** A mock with no
      stated trade-off is a picture, not a candidate.
- [x] ⚠ **Every mock is shown under the hard cases too, not only the tidy one.** This is the acceptance
      item that decides whether the story is useful:
      - a long run (dozens of steps) that cannot fit the viewport;
      - deep nesting;
      - a sub-agent fan-out (several concurrent children);
      - a narrow terminal, at and below the width where it stops working.
      A layout that looks best on a tidy six-step flow and collapses on a real run is exactly the wrong
      answer, and it is the one that wins a side-by-side unless the hard cases are rendered too.
- [x] Elision is visible wherever a mock elides — the same honesty rule the epic places on the real
      view. A mock that silently truncates teaches the wrong lesson to whoever implements from it.
- [x] ⚠ **The story ends in a recommendation**, written down: which layout to build in A-137, why, and
      what it gives up. Five pictures and no decision is the failure mode.
- [x] Nothing here wires live events, and nothing ships in the default TUI path. It is a comparison
      artifact.
- [x] Full gate green.

## Notes

- Reuse what exists rather than reimplementing: `crates/flux-tui/src/plan.rs` already renders a
  `flow.plan` observation as a styled DAG (preferring `plan_ast`, syntax-highlighted through
  `flux_flow::render::render_styled`), and `panes.rs`/`toolview.rs` hold the current drawing
  vocabulary. ⚠ The graph-first mock in particular should build on `plan.rs`, since that renderer is
  what A-138 will use for real.
- The loop vocabulary to represent is already fixed by `UiEvent` (`crates/flux-tui/src/controller.rs:8`):
  `Planning`, `Plan`, `Phase`, `Intent`, `ToolCall`, `ToolTiming`, `ToolResult`, `ToolProgress`,
  `CallUsage`, `SpawnActivity`. Mock the shapes that exist rather than inventing new ones.
- ⚠ Leave room for controls without building them: [A-140](A-140-pause-a-live-run.md) wants a pause
  affordance attached to a step, and [A-142](A-142-inspect-a-paused-run.md) wants an inspection pane.
  A layout with nowhere to put either is disqualified — worth noting per mock.
- Keeping it hard-coded is what makes it cheap. If a mock starts needing real events, that is a sign it
  belongs in A-137 instead.

## Progress

- Filed 2026-08-01 and dispatched the same day.
- **Built 2026-08-01.** `crates/flux-tui/src/loopmock/` — one hand-authored fixture
  (`fixture.rs`) and five renderers (`thread.rs`, `tree.rs`, `split.rs`, `timeline.rs`,
  `graph.rs`). Look at them with `cargo run -p flux-tui --example loop_mocks` (interactive:
  `1`–`5` mock, `t l d f` load case, `w n m` width, `?` the recommendation) or read the committed
  side-by-side set at **[docs/designs/agent-loop-visibility-mocks.md](../designs/agent-loop-visibility-mocks.md)**
  — 30 renders, regenerated by `FLUX_UPDATE_GOLDEN=1 cargo test -p flux-tui --test loop_mocks`.
- Nothing is wired: `crates/flux-tui/tests/loop_mocks.rs` asserts no file under `src/` other than
  `lib.rs`'s `pub mod` line mentions the module, so it cannot drift into the live path.
- The mechanics that make the comparison mean something, rather than being five drawings:
  - **one fixture, four load cases** — tidy (15 steps), long run (49 across nine phases), deep
    nesting (8 levels, a sub-agent that spawns a sub-agent), fan-out (6 concurrent workers) —
    each drawn at 100×28, 52×20, 30×14, and at `min_cols` and `min_cols - 1`;
  - **elision is derived, not declared**: `Tally` computes `hidden = total - drawn` centrally, so
    a renderer cannot under-report, and a test asserts every elision's marker text is on screen;
  - **one shared scroll policy** (`window`), so the winner is decided by layout and not by whose
    scrolling happened to be cleverer;
  - **every line leaves through `clip`**, so "no mock overflows its terminal" is a property;
  - the graph mock annotates `plan::render`'s real output — the renderer A-138 will use — and a
    test fails if the gutter and that renderer stop lining up.

### The recommendation

**Build the split (mock 3), with the flat thread (mock 1) as its sub-64-column fallback.**

The long-run row of the matrix decides it. At 49 steps the thread and the tree have both scrolled
23 rows away from the start; the timeline shows bars with no content; the graph shows a program
that does not contain the running phase at all. The split is the only layout still showing the
whole run — all nine phases, because a finished phase stays one row — *and* the running command
*and* its timing at once. On deep nesting it is the only one showing the model call's token cost
beside the chain that produced it. Both follow from the same property: the rail's height is set
by the phase count and the pane's content by one step, so neither grows with the run. It is also
the only layout where A-140's pause has an unambiguous home and A-142's inspection pane already
exists rather than needing to be added later.

What it gives up: **width, badly** — 64 columns, and the snapshots show it refusing at 52 in every
case while all four others still draw, which is why the fallback is part of the recommendation
rather than an afterthought; **content in the rail** (at 40 rail columns a leaf is `→ read 7.4s`,
so the fan-out case shows six workers but what only *one* is doing — the tree shows all six);
**one narrative**, which answers the design doc's open question in favour of the pane; and
**focus state**, the input handling and follow-the-running-step policy no other mock needs.

Runner-up is the tree — the most legible drawing of a run's shape at 100 columns, lost to the
indentation tax. Its connectors are worth keeping, and the split's pane already uses them.

Do not build mock 5 as the top-level view. It is right for A-138's *expansion* and wrong for a
live view, and its own snapshots show why twice: the program does not move, so most rows are
always the parts that are not running; and mapping runtime steps back onto program nodes
misattributes the moment a sub-agent runs a program of its own — in the fan-out case six workers'
`glob` calls land on the parent's `glob` node. A-138 should expand one *step* into its graph,
which is exactly what the split's pane has room for.
