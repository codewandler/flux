---
id: A-138
title: "Expand a step into its execution graph — the moment the architecture becomes visible"
pillar: Agent
status: backlog
design: docs/designs/agent-loop-visibility.md
epic: agent-loop-visibility
areas: [flux-tui, flux-lang]
note: "reuses what exists: crates/flux-tui/src/plan.rs already renders a flow.plan observation as a styled DAG, preferring plan_ast so the tree is syntax-highlighted via flux_flow::render::render_styled. Do not grow a second plan renderer"
---

# Show the program the model is being called from

## Goal

Expanding the current step shows its execution graph — down to the Flux-Lang the runtime is running —
so an observer can see the model call as one typed node inside an authored program.

## Why this is the point of the epic

flux's claim is that the LLM is not the runtime. Everywhere else that is an assertion. Here it is
visible: the graph is the program, and the model call is a node in it.

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
