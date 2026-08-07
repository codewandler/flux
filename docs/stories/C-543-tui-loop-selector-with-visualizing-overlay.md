---
id: C-543
title: "Select the agent's loop from the TUI with a hotkey and a visualizing overlay"
pillar: Core
status: ready
priority: 31
epic:
design: docs/designs/agent-loop-harnesses.md
areas: [flux-tui, flux-cli, flux-app]
depends_on: [C-569]
note: "display/switch the resolved binding, not an ambient filename; admitted Fleet workers change only through explicit re-admission"
---

# Select the agent's loop from the TUI with a hotkey and a visualizing overlay

## Goal

The loop driving the selected agent becomes a first-class, visible, switchable thing in the TUI:
the current loop's name is always shown, a hotkey opens a loop selector listing the available
`*.flux` loops, and selecting one shows a short overlay that visualizes the outer loop's structure
and renders its description — so an operator can see and change what loop an agent runs without
leaving the TUI.

## Acceptance

- [x] The current resolved loop profile, revision and abbreviated digest are visible in the TUI for
      the selected agent. *(First pass — confirm where: status bar, agent header, or both.)*
      Resolved: the **header bar**, as one droppable segment (`loop adaptive@1 8f3c…`) ranked
      directly below the budget segment — the harness outranks cumulative counters.
- [x] A hotkey opens a loop selector listing every available `*.flux` loop; choosing an entry
      switches the selected agent's loop. A failing-first test proves the switch takes effect.
- [x] On selection, a short overlay renders (a) a visualization of the outer loop's structure and
      (b) the loop's description; a failing-first TUI test proves the overlay content.
- [x] Selection changes the next permitted start/turn under C-569's lifecycle rules. It cannot
      silently change the snapshotted loop of an admitted/running Fleet worker; that path offers an
      explicit re-admission/new-session action or refuses.
- [x] The selector reflects the live set of loop files — a loop added while the TUI runs appears
      without restart (this is what [C-544](C-544-create-agent-loops-by-prompting.md) relies on).
- [ ] The gate is green in both workspaces.

## Progress

- 2026-08-06 — implemented. `F3` opens the selector over a fresh scan of the workspace's
  `.flux/loops` directory (plus the shipped `adaptive@1` preset, always offered); Enter resolves the
  chosen file into a real `AgentLoopBinding` — profile from the file stem, entry point from the
  parsed flow, digest over the exact admitted bytes — and hands it to the engine through the new
  `FlowEngine::adopt_agent_loop_binding`, which reloads the executable loop *and* its identity
  together after re-validating runtime features and operations. The header shows the resolved
  binding, never a filename. A session that has already started a turn is admitted (C-569): the
  selector refuses and names the new-session/re-admission path instead of switching a running agent,
  and the engine's own `begin_turn_lifecycle` check remains the backstop. The selection overlay
  renders the loop's leading comment block as its description and its outer statements through
  Flux-Lang's own statement renderer, so it visualizes the program that will run.
- 2026-08-06 — the new binding's two contract consequences settled: `website/docs/agent/tui.md`
  documents the header's loop segment and the `F3` row (`tui_page_documents_the_bound_keys_and_themes`
  ties that page to `HELP_KEYS`, so a bound key that is not on the page fails the gate), and
  `help_overlay_lists_keys_and_all_commands` draws at 80x28 — one `HELP_KEYS` row taller, the overlay
  no longer fits 26 rows and was clipping its last command line rather than a key.
- Remaining: the full repository gate in both workspaces runs at the wave integration boundary, not
  from the story worktree. The committed public-documentation mirror
  (`crates/flux-server/assets/public-docs.zip`) is rebuilt on the assembled candidate, not here.

## Notes

- Filed 2026-08-05 via /track:story from the roadmap coordinator session; first-pass draft from the
  title — refine the hotkey, overlay layout, and where the loop name lives before dispatch.
- Sibling: [C-544](C-544-create-agent-loops-by-prompting.md) adds prompt-driven loop creation and
  depends on this selector.
- C-569 owns binding identity and lifecycle; this story is only its human selection/projection.
