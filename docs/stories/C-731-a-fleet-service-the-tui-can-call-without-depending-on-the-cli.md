---
id: C-731
title: "A fleet service the TUI can call without depending on the CLI"
pillar: "Core"
status: backlog
priority: 2
epic: delivery-is-verified
areas: [flux-cli]
note: "board_fleet_cmd.rs is 24973 lines holding the whole Fleet product, and flux-tui is forbidden from depending on flux-cli (C-518), so the TUI cannot reuse any of it. flux-cli currently reaches UP to L6 to implement flux_tui::operations::FleetBoardSource in a 692-line impl, which is the dependency backwards: the surface owns the port and its only implementation lives in a binary crate nothing may depend on. Extract a new internal crate flux-fleet at L3 holding the view types and the port, so both flux-cli and flux-tui call it. Plan at ~/.claude/plans/fizzy-baking-owl.md"
---

# A fleet service the TUI can call without depending on the CLI

## Goal


## Acceptance

- [ ] Define acceptance.
