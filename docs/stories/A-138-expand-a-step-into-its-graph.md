---
id: A-138
title: "Expand a step into its execution graph — the moment the architecture becomes visible"
pillar: Agent
status: blocked
design: docs/designs/agent-loop-visibility.md
epic: agent-loop-visibility
areas: [flux-tui, flux-lang]
note: "⚠ PREMISE UNDERMINED by A-145: the captured real session accepted 127 plans, EVERY ONE a single op, and `plan_ast` is never persisted at all — so there was no DAG to draw and A-144's graph mock pictured a payload the log does not contain. Re-scope or establish that a real authored flow does produce one, BEFORE building"
---

# Show the program the model is being called from

## Goal

Expanding the current step shows its execution graph — down to the Flux-Lang the runtime is running —
so an observer can see the model call as one typed node inside an authored program.

## Why this is the point of the epic

flux's claim is that the LLM is not the runtime. Everywhere else that is an assertion. Here it is
visible: the graph is the program, and the model call is a node in it.

## ⚠ A-145 found there may be nothing to draw

Blocking, and it must be settled before any of the Acceptance below is worth attempting. Driving the
mocks from a real recorded session (191 steps, 9 turns) found:

- the session accepted **127 plans, every one a single op** — no DAG, nothing to expand into;
- **`plan_ast` is never persisted**, so A-144's syntax-highlighted graph mock pictured a payload the log
  does not contain;
- rendered graph-first, that run draws **one line and hides 189 of 191 steps**.

⚠ The distinction that decides this story: is `plan_ast` absent from **the log**, from **that capture**,
or from **adaptive runs only**? An *authored* flow — `flux flow run` over a real `.flux` file — plausibly
does produce a multi-node plan, and the demo case is an authored loop. **Establish which, first.** If
the answer is "adaptive runs are single-op by construction", this story is about authored flows only and
should say so in its title.

## Acceptance

- [ ] **Failing-first**: a test asserting an expanded step renders its DAG, failing at the merge base.
- [ ] ⚠ **Reuses `crates/flux-tui/src/plan.rs`**, which already renders a `flow.plan` observation as a
      styled ratatui block and prefers `plan_ast` so the tree is syntax-highlighted through
      `flux_flow::render::render_styled`. A second plan renderer would drift from the first within a
      release, and then two surfaces would disagree about what the program is.
- [ ] The model call is identifiable as a node — the whole argument depends on seeing it *inside* the
      graph rather than beside it.
- [ ] ⚠ **A floor below which the view refuses rather than misleads.** A DAG squeezed into a narrow
      terminal that silently drops nodes is worse than a message saying it does not fit — especially
      in the setting this is built for.
- [ ] Full gate green.

## Notes

- Depends on [A-137](A-137-the-step-thread.md) for the thread it expands within.
- `flux-render` (L-74…L-78) renders flux source and plans to SVG/PNG. Not this surface, but if the
  layout logic is shareable, share it rather than deriving a third.
- ⚠ An authored flow and a host-built plan are both `flow.plan` observations. Check that both render,
  or say which does not — the demo case is an authored loop.

## Progress

- Filed 2026-08-01 with the agent-loop-visibility epic.
