# Design: Watch the agent think — the loop as a live thread, expandable down to the graph

**Status:** proposed · **Pillar:** Agent · **Stories:** [A-144](../stories/A-144-five-tui-mocks-of-one-flow.md) · [A-137](../stories/A-137-the-step-thread.md) · [A-138](../stories/A-138-expand-a-step-into-its-graph.md) · [A-139](../stories/A-139-the-loop-view-under-load.md)

> **The layout is chosen, by looking at it.** [A-144](../stories/A-144-five-tui-mocks-of-one-flow.md)
> drew five hard-coded layouts of one flow under four load cases and three widths; the side-by-side
> set is [agent-loop-visibility-mocks.md](agent-loop-visibility-mocks.md), and its recommendation is
> the **split** (a condensed rail whose height tracks the phase count, plus a detail pane) with the
> flat thread as the sub-64-column fallback. That answers this doc's open "pane or inline" question
> in favour of the pane, and it settles where A-140's pause and A-142's inspection pane live before
> A-137 commits to a shape. Nothing there is wired to a live run.

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

## ⚠ The five are not five candidates — they are points in a space with three axes

Owner-directed after driving the mocks, 2026-08-02, and it supersedes "pick one of five":

> *"they are all quite good — what would be nice would be the option to \[choose\] nesting level to
> display, and that things which happened already could be condensed. Then also an optional detail
> view."*

Three controls, and they are **orthogonal**:

| axis | what it does | which mock it turns you into |
|---|---|---|
| **depth limit** | how many nesting levels are drawn | flat thread ↔ tree |
| **condense completed** | finished work collapses to one row | the long-run win |
| **detail pane** | optional, toggled — not always-on | tree ↔ split |

Compose them and the mocks stop being rivals: **the flat thread with condensing and a depth limit and
an optional pane *is* the split.** The tree is the same thing at depth ∞ with the pane off. So A-137
should build the **axes**, not adopt a picture — and a user who wants any of the five can have it.

⚠ **This converges with what the review independently found**, from the other direction. A-144's
comparison was confounded because only mock 3 had phase-condensing, which forced the recommendation to
*"condense finished phases first, then build the split"* — the two being separable was the finding.
The owner's read says the same thing and goes further: **all three are separable**, and separating them
is the design rather than a caveat on it.

Two consequences worth stating, because they are what this buys:

- **The floors move.** The split's cost was a 64-column, 10-row floor, nearly double the others'. With
  the pane *optional*, that floor applies only when it is on — the fallback stops being a second layout
  and becomes the same layout with a toggle off.
- ⚠ **Defaults are now the real decision, and they are where honesty lives.** A default depth that hides
  a sub-agent's work, or condensing that swallows a failed step, would make the view flatter than the
  run. Every axis must default to showing *more* than it hides, and elision stays visible at every
  setting — the property A-144's rework made unconditional.

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
