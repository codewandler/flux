---
id: A-144
title: "Five hard-coded TUI mocks of the same flow — pick the loop view by looking at it, not by arguing about it"
pillar: Agent
status: done
design: docs/designs/agent-loop-visibility.md
epic: agent-loop-visibility
areas: [flux-tui]
note: "⚠ The recommendation CHANGED under review: condense finished phases FIRST, then build the split — the long-run comparison was confounded in mock 3's favour, since only it had phase-condensing. The two are separable and the first is most of the win. Snapshot set: docs/designs/agent-loop-visibility-mocks.md"
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
  — 50 renders, regenerated by `FLUX_UPDATE_GOLDEN=1 cargo test -p flux-tui --test loop_mocks`.
- Nothing is wired: `crates/flux-tui/tests/loop_mocks.rs` walks `src/` recursively and asserts no
  file but `lib.rs`'s `pub mod` line mentions the module, so it cannot drift into the live path.
- The mechanics that make the comparison mean something, rather than being five drawings:
  - **one fixture, four load cases** — tidy (15 steps), long run (49 steps under nine top-level
    rows), deep nesting (8 levels, a sub-agent that spawns a sub-agent), fan-out (6 workers, 5 of
    them running at once) — each drawn across the **whole viewport envelope `render` accepts**:
    18 widths × 28 heights per mock per case in the property tests, and 10 renders per mock in
    the committed set;
  - **elision is derived, not declared**: `Tally` computes `hidden = total - drawn` centrally, so
    a renderer cannot under-report, *and* `Tally::finish` forces any recorded marker that did not
    survive composition back onto the screen — so "whatever it withholds, it says so" is
    unconditional rather than true-for-the-sizes-that-were-tested;
  - **two floors per layout**, `min_cols` and `min_rows`, both drawn at and one step under. A
    layout's cost is two numbers, and mock 3's row floor is nearly twice the others';
  - **one shared scroll policy** (`window`), so the winner is not decided by whose scrolling
    happened to be cleverer — ⚠ but see the confound below, which the shared policy does *not*
    cover;
  - **every line leaves through `clip`**, so "no mock overflows its terminal" is a property;
  - the graph mock annotates `plan::render`'s real output — the renderer A-138 will use — and a
    test fails if the gutter and that renderer stop lining up.

### The recommendation

**Condense finished phases first. Then build the split (mock 3), with the flat thread (mock 1) as
its sub-floor fallback.** The two are separable, and the first is most of the win.

⚠ **The comparison is confounded, in mock 3's favour, and the conclusion is stated to survive
that.** Only mock 3 was given progressive disclosure: its rail pre-filters to top-level steps plus
the focused subtree (`split.rs`, `rail_rows`) before the shared scroll policy runs, while mocks 1
and 2 draw one row per step at every depth by construction. So the long-run picture — mock 3
holding the whole run while the thread and the tree sit at `↑ 23` — measures **condensing**, not
**two columns**. A condensed one-column thread would hold it too: nine top-level rows plus the
running phase's four children, thirteen rows against 49 steps. That is measured, not asserted, by
`condensing_and_not_the_second_column_is_what_makes_the_long_run_fit`. **A-137 should take the
cheap lesson first** — it is also what A-137 already proposed ("completed steps condense to a
single line").

What the second column buys *after* discounting the confound, from the tidy and deep-nesting cases
rather than the long-run one: it is the only layout that shows a model call's token cost and its
streaming output tail beside the chain that produced it; A-142's inspection pane does not need
building because the right column already is it; and A-140's pause has an unambiguous home on the
focused rail row. The other four answer "where does the pane go" with a sheet or a bottom split,
so A-142 would relayout whatever A-137 shipped.

What it gives up: **width and height, badly** — 64 cols × 10 rows, both the highest floors of the
five, and the snapshots show it refusing at 52 columns in every case while all four others still
draw, which is why the fallback is part of the recommendation rather than an afterthought;
**content in the rail** (at 40 rail columns a leaf is `→ read 7.4s` — the op and its timing, with
the argument gone, so the fan-out case shows all six workers' current ops but only one's
arguments, where the tree shows all six with theirs); **one narrative**, which answers the design
doc's open question in favour of the pane; and **focus state**, the input handling and
follow-the-running-step policy no other mock needs.

Runner-up is the tree — the most legible drawing of a run's shape at 100 columns and the only one
where a fan-out looks like a fan-out with every argument visible, lost to the indentation tax. Its
connectors are worth keeping, and the split's pane already uses them. ⚠ The confound above cuts
against the tree harder than against any other mock; condensed, it would be a much closer call.

Do not build mock 5 as the top-level view. It is right for A-138's *expansion* and wrong for a
live view, and its own snapshots show why twice: the program does not move, so most rows are
always the parts that are not running; and mapping runtime steps back onto program nodes
misattributes the moment a sub-agent runs a program of its own — in the fan-out case six workers'
`glob` calls land on the parent's `glob` node. A-138 should expand one *step* into its graph,
which is exactly what the split's pane has room for.

### Rework round 1 (2026-08-02)

- **Blocking, fixed.** The module documented "whatever a render withholds, it says so on screen"
  unconditionally, but at `rows == 6` mock 3's pane assembled six lines while `draw` composed four,
  so its `+N more` marker was cut while `Tally` still recorded the elision — a silent truncation,
  the one thing the artifact claims not to do. Fixed in three layers: `MockSpec::min_rows` so the
  degenerate viewport is refused rather than papered over; `pane_rows` now sized against
  `PANE_CHROME + 1` so it cannot outgrow its budget; and `Tally::finish` forces any recorded marker
  that did not survive composition back onto the screen, making the property unconditional for
  every renderer, present and future. Pinned by
  `a_short_terminal_cannot_cut_the_split_panes_own_elision_marker`.
- **The test was a guard tested against its own assumptions.** It swept the three viewports the
  snapshot set had chosen, so it agreed with the artifact rather than with the envelope `render`
  accepts. It now sweeps 18 widths × 28 heights — every mock's floor and one under it included —
  which is what found nothing else.
- The "five strategies" test is renamed and documented as a **regression guard, not evidence**;
  the judgement belongs to the snapshot set.
- Count corrections: "nine phases" → nine top-level rows (a plan step plus eight phases); "6
  concurrent workers" → 6 workers, 5 running; and the rail undersell about what the fan-out shows.
- The golden guard now **refuses** an unrecognized `FLUX_UPDATE_GOLDEN` value instead of silently
  checking, matching `crates/flux-lang/tests/support/golden_mode.rs`; AGENTS.md's enumeration of
  checked-in-golden guards is updated from three to four.
- `the_mocks_stay_out_of_the_live_tui_path` now walks `src/` recursively.
