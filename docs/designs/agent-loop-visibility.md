# Design: Watch the agent think — the loop as a live thread, expandable down to the graph

**Status:** proposed · **Pillar:** Agent · **Stories:** [A-144](../stories/A-144-five-tui-mocks-of-one-flow.md) · [A-145](../stories/A-145-a-real-run-as-the-mock-fixture.md) · [A-146](../stories/A-146-three-axes-not-five-pictures.md) · [A-137](../stories/A-137-the-step-thread.md) · [A-138](../stories/A-138-expand-a-step-into-its-graph.md) · [A-139](../stories/A-139-the-loop-view-under-load.md)

> **The layout is chosen, by looking at it.** [A-144](../stories/A-144-five-tui-mocks-of-one-flow.md)
> drew five hard-coded layouts of one flow under four load cases and a swept viewport envelope; the
> side-by-side set is [agent-loop-visibility-mocks.md](agent-loop-visibility-mocks.md). Its
> recommendation, after review found the long-run comparison confounded in the split's favour, is:
> **condense finished phases first, then build the split** (a condensed rail plus a detail pane),
> with the flat thread as the sub-64-column fallback. That answers this doc's open "pane or inline"
> question in favour of the pane, and it settles where A-140's pause and A-142's inspection pane
> live before A-137 commits to a shape.
>
> **[A-145](../stories/A-145-a-real-run-as-the-mock-fixture.md) re-checked that against a real
> recorded run** — nine turns, 33 minutes, 191 steps out of `~/.flux/events.db` — and **confirmed
> the headline with three corrections**, all of which A-137 has to budget for. They are stated in
> full at the top of the recommendation in the snapshot set; in short:
>
> 1. **Condensing is sub-linear, not constant, and its win is concentrated.** 191 steps → a 25-row
>    rail (7.6×), but the depth-0 unit of a real *session* is a turn and turns accumulate, and the
>    phase sizes are lumpy: 36 of 55 real phases are a **single step** (condensing them saves
>    nothing) while one is **57** (which is where the whole win is). The hand-authored fixture's
>    evenly-sized 3–6-step phases made the saving look uniform.
> 2. **The split and the flat thread hide the same number of steps** on the real run (166 of 191).
>    The split's advantage is *coverage* — all nine turns plus the current one in full — not
>    capacity. Still the right call; the old phrasing ("the only one that still shows the whole
>    run") was an artifact of a fixture whose run was one run rather than nine turns.
> 3. **Real nesting is three levels, not eight, and mock 5 has nothing to draw.** The indentation
>    tax that cost the tree the comparison was charged against an invented depth this log has never
>    recorded, so the condensed tree is now the honest runner-up. Meanwhile the captured session
>    accepted **127 plans and every one of them is a single op** — there is no authored DAG for a
>    graph-first view to be about, and `plan_ast` is never persisted at all.
>
> ⚠ And the fidelity table the reconstruction produced — what the durable log can and cannot
> rebuild — is in the same document, and is the start of what [C-422](../stories/C-422-the-render-projection.md)
> owes.
>
> **[A-146](../stories/A-146-three-axes-not-five-pictures.md) then tested whether the five are
> really three knobs** — and the headline changed again. **A-137 should build the axes and ship
> `depth All · condense on · pane off`**, but *not* on the reasoning that the axes reproduce the
> five: they reproduce the tree exactly, they cannot reach the split at all, and the flat thread and
> the tree turn out to be the same point. The correction, with the claim it corrects named as mine,
> is in "Three axes — the claim, and the measurement that partly refuted it" below.

## Why

flux's central claim is that **the LLM is not the runtime** — authored control flow and a deterministic
Rust runtime decide what happens, and the model fills bounded semantic slots. That is an architectural
claim, and architecture is invisible. In a demo or a review it lands as an assertion.

A live view of the loop makes it *observable*: a thread of steps where what is finished is condensed to
one line, the current step is expanded, and expanding further shows the **actual execution graph** —
down to the Flux-Lang the runtime is executing. At that point nobody has to be told the model is not
driving; they can see the program it is being called from.

### Most of the rendering already exists

- `crates/flux-tui/src/plan.rs` already renders a `flow.plan` observation — *"a durable authored or
  host-built execution DAG"* — as a styled ratatui block, preferring the observation's `plan_ast` so
  the tree is **syntax-highlighted** through `flux_flow::render::render_styled`.
- `UiEvent` already carries `Plan`, `Phase`, `Planning`, `Intent`, `ToolCall`, `ToolTiming`,
  `ToolResult`, `ToolProgress`, `CallUsage` and `SpawnActivity` — the loop's whole vocabulary is
  already on the wire to the UI.

So this epic is largely a **view** problem, not an instrumentation problem: the events exist and the
graph renderer exists. What is missing is the progressive-disclosure thread that ties them together and
stays readable while a run is moving.

## ⚠ Once it reads the event store, it stops being a mock

Owner-directed, 2026-08-01, and it changes what this epic is building. A-144's mocks ran on a
hand-authored fixture — a throwaway. [A-145](../stories/A-145-a-real-run-as-the-mock-fixture.md) drives
them from a run **reconstructed out of the event database**, and at that point they are not mocks at
all: they are renderers over real recorded state, and whichever wins is the seed of the shipped view.

⚠ **Three epics now need the same primitive**, and building it three times would be the expensive
mistake:

| who | needs | story |
|---|---|---|
| this epic | render a real run's timeline to compare layouts | A-145 |
| session-screencast | render a recorded session to an asciicast | [C-422](../stories/C-422-the-render-projection.md) |
| interactive-debugger | show what a paused run is holding | [A-142](../stories/A-142-inspect-a-paused-run.md) |

**[C-422](../stories/C-422-the-render-projection.md) is the shared foundation** and its finding is the
one that matters here: the durable record is rich — `ts` at millisecond resolution, `EventKind` holding
messages, turns, plans and (since C-43) durably-redacted op output — while the TUI's existing
durable→screen path is **100 lines handling five observation kinds against 26 live `UiEvent`
variants**. The data is largely there; the projection is not.

⚠ So a reconstruction written for the mocks must not become a second, competing one. What A-145 finds
it can and cannot rebuild **is** the beginning of C-422's fidelity table, and anything the log cannot
supply is a finding about the *record*, not a gap in a mock.

## ⚠ Three axes — the claim, and the measurement that partly refuted it

Owner-directed after driving the mocks, 2026-08-02, and it superseded "pick one of five":

> *"they are all quite good — what would be nice would be the option to \[choose\] nesting level to
> display, and that things which happened already could be condensed. Then also an optional detail
> view."*

Three controls, claimed to be **orthogonal** and to span the five:

| axis | what it does | which mock it was claimed to turn you into |
|---|---|---|
| **depth limit** | how many nesting levels are drawn | flat thread ↔ tree |
| **condense completed** | finished work collapses to one row | the long-run win |
| **detail pane** | optional, toggled — not always-on | tree ↔ split |

The claim attached to that table was mine, stated here as fact on my own say-so: *"the flat thread
with condensing and a depth limit and an optional pane **is** the split; the tree is the same thing at
depth ∞ with the pane off."*

⚠ **[A-146](../stories/A-146-three-axes-not-five-pictures.md) built the three controls and tested it.
Half of it is wrong, and this section is a correction rather than an unmarked reversal.** The controls
were swept over all twelve combinations × four load cases × the same 18×28 viewport envelope the five
mocks are held to; the tests are in `crates/flux-tui/tests/loop_mocks.rs` and the pictures in
[agent-loop-visibility-mocks.md](agent-loop-visibility-mocks.md). What survived:

1. ✅ **The tree is a point in the space**, exactly: `depth 6 · condense off · pane off`, on every load
   case, 6 being `plan.rs`'s `MAX_TREE_DEPTH`. And the flat thread's *view* is `depth ∞ · condense off ·
   pane off`, also exactly.
2. ❌ **The depth limit does not move between the thread and the tree — they are the SAME POINT.**
   Both draw every step at every depth; on both recorded cases their step sets are identical, and
   neither hides anything the other shows. What separates them is **indentation**, a drawing decision
   no show/hide control can express, and therefore a *fourth* axis if A-137 wants both pictures. My
   table assigned the depth limit a job that is not its job. (It is still a real control — it is what
   makes a deep sub-agent fan-out readable — but A-145 measured real nesting at three levels, so on a
   real run it never has to fire.)
3. ❌ **The split is not reachable at all.** The axes reach it only when the run has **one top-level
   step**, or the terminal is too short for the split to draw its own rule. On the real nine-turn
   session there are 24 viewports where the split both draws its whole rail and withholds something,
   and **zero** matches in them. The structural reason: mock 3's rail is *one row per top-level step
   plus the focused top-level step's entire subtree, completed work included* — so it discriminates on
   **focus**, while condensing discriminates on **status**. With one root the two rules coincide; with
   nine turns they cannot.

⚠ **So what A-137 owes is not a fourth picture but a fourth decision: condensing's GRANULARITY.**
"Finished work collapses to one row" does not say *at what level*, and the answer changes what you
get — fold uniformly and you get every turn's shape at one row per phase; fold only at the top level
and you get mock 3's rail. A one-bit toggle cannot express the difference, and it should be decided
explicitly rather than inherited from whichever gets written first.

⚠ **The claim did converge with A-144's review on the part that held.** That comparison was confounded
because only mock 3 had phase-condensing, which forced the recommendation to *"condense finished phases
first, then build the split"* — the two being separable was the finding, and separability is confirmed.

Three consequences, all now measured rather than predicted:

- **The floors move, and the measurement retires a claim.** A-144 charged the split a 64-column,
  10-row floor and recommended the flat thread as a separate sub-64-column *fallback*. That floor is
  the **pane's**: with the pane off the composed view draws every load case at 40×6, the lowest floor
  of the five, and with it on it refuses one column under 64 exactly as mock 3 does. **The fallback is
  not a second layout; it is this layout with a toggle off.**
- ⚠ **Defaults are the real decision, and the measured answer is `depth All · condense on · pane off`.**
  Depth `All` withholds nothing; condensing folds only finished *and wholly successful* subtrees, each
  into a row carrying `+N`; the pane is off because it owns the 64-column floor and at 52 columns —
  where most of this is read — pane-on refuses entirely while pane-off draws.
- ⚠ **The honesty worry inverted.** The risk this epic carried was that condensing would swallow a
  failed step. On the real run the opposite happens: **condensing is what buys the room to show one.**
  Session `s_1477` turn 7's `git_stage` failed `exit 128` inside an `execute_batch` that then closed
  `ok` — a `Done` parent holding a `Failed` child, a shape nobody would have authored. Uncondensed it
  is 166 steps back and off screen; condensed, the clean turns fold, turn 7 *refuses to fold because it
  holds a failure*, and the `✗` is visible. Elision stays visible at every setting, and a depth limit
  reports how many **levels** it withheld and how many of the withheld steps failed.

## Approach

Three stories, smallest first, each independently useful.

### A-137 — the step thread

One line per step. Completed steps condense to a single line (what it was, how it went, how long).
The current step expands to show what is happening inside it. This is the whole feature at its most
useful, and it is worth shipping alone.

### A-138 — expand a step into its graph

Expanding the current step further shows the execution DAG for it, reusing `plan.rs`'s existing
renderer and syntax highlighting rather than growing a second way to draw a plan. This is the moment
the architecture becomes visible: the audience sees authored Flux-Lang, with the model call as one
typed node inside it.

### A-139 — the loop view under load

⚠ The view must not become the bottleneck or the liar. A fast loop emits events faster than a terminal
can usefully redraw, and a sub-agent fan-out multiplies that. Bounded redraw, bounded history, and
honest elision — *"a view that silently drops steps is worse than one that says it is behind."*

## Alternatives considered

- **A separate web UI.** More layout freedom, and it would render a DAG far better than a terminal.
  Rejected as the starting point: the TUI is where the daily driver already is, the plan renderer is
  already there, and a second surface would drift from the first.
- **Log-style scrollback only.** Cheapest, and it is roughly what exists. Rejected: scrollback shows
  *what happened* and this epic exists to show *what is happening* and how it is structured.
- **Always-expanded.** Simpler, no disclosure state. Rejected: a real run's graph does not fit, and a
  view that overflows is a view nobody reads.

## Risks & open questions

- ⚠ **A visualization that flatters.** Rendering the loop as tidier than it is would be a lie told in
  exactly the setting where it is most persuasive. Elision must be visible.
- **Terminal size.** The graph view has a floor below which it misleads. Decide what happens under it.
- **Open:** whether the thread is scrollback-inline or a pane. A pane keeps the transcript readable and
  costs width; inline keeps one narrative and fights the transcript.
- **Open:** how sub-agent activity (`SpawnActivity`, the fleet pane) folds in — one thread per agent, or
  a nested thread? The fleet projection already exists and should not be duplicated.
- ⚠ **This is where a pause hotkey wants to live** ([run-control](run-control.md)) and where a debugger
  would render ([interactive-debugger](interactive-debugger.md)). Do not design the thread so that
  attaching a control to a step later means rewriting it — but do not build the controls here either.

## Acceptance / done

- A running agent shows a live thread: finished steps condensed to one line, current step expanded.
- Expanding the current step shows its execution graph, syntax-highlighted, from the same renderer the
  plan view already uses.
- The view stays responsive and honest under a fast loop and under sub-agent fan-out; elision is
  visible.
- Someone watching a demo can see that the model is called *from* a program, without being told.
