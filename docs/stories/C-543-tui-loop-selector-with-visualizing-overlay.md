---
id: C-543
title: "Select the agent's loop from the TUI with a hotkey and a visualizing overlay"
pillar: Core
status: ready
priority: 31
epic:
design:
areas: [flux-tui, flux-cli, flux-app]
note: "current loop name always visible; a hotkey opens a selector; choosing a loop shows a short overlay that visualizes the outer loop and renders its description"
---

# Select the agent's loop from the TUI with a hotkey and a visualizing overlay

## Goal

The loop driving the selected agent becomes a first-class, visible, switchable thing in the TUI:
the current loop's name is always shown, a hotkey opens a loop selector listing the available
`*.flux` loops, and selecting one shows a short overlay that visualizes the outer loop's structure
and renders its description — so an operator can see and change what loop an agent runs without
leaving the TUI.

## Acceptance

- [ ] The current loop's name is visible in the TUI for the selected agent. *(First pass — confirm
      where: status bar, agent header, or both.)*
- [ ] A hotkey opens a loop selector listing every available `*.flux` loop; choosing an entry
      switches the selected agent's loop. A failing-first test proves the switch takes effect.
- [ ] On selection, a short overlay renders (a) a visualization of the outer loop's structure and
      (b) the loop's description; a failing-first TUI test proves the overlay content.
- [ ] The selector reflects the live set of loop files — a loop added while the TUI runs appears
      without restart (this is what [C-544](C-544-create-agent-loops-by-prompting.md) relies on).
- [ ] The gate is green in both workspaces.

## Progress

- (not started)

## Notes

- Filed 2026-08-05 via /track:story from the roadmap coordinator session; first-pass draft from the
  title — refine the hotkey, overlay layout, and where the loop name lives before dispatch.
- Sibling: [C-544](C-544-create-agent-loops-by-prompting.md) adds prompt-driven loop creation and
  depends on this selector.
