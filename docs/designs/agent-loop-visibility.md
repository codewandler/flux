# Design: Watch the agent think — the loop as a live thread, expandable down to the graph

**Status:** proposed · **Pillar:** Agent · **Stories:** [A-137](../stories/A-137-the-step-thread.md) · [A-138](../stories/A-138-expand-a-step-into-its-graph.md) · [A-139](../stories/A-139-the-loop-view-under-load.md)

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
