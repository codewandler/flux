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

`board_fleet_cmd.rs` is ~25,000 lines holding the entire Fleet product, and `flux-tui` is forbidden
from depending on `flux-cli` (C-518). So the TUI cannot reuse any of it.

What happened instead is the dependency backwards: `flux-cli` reaches **up** to L6 to implement
`flux_tui::operations::FleetBoardSource` in a 692-line impl. The surface owns the port, and the
port's only implementation lives in a binary crate that nothing is allowed to depend on. Any second
consumer — the server, the SDK, a test harness — has no way in at all.

Extract an internal crate `flux-fleet` at **L3** holding the view types and the port, so `flux-cli`
and `flux-tui` both call down into it rather than one reaching up into the other.

## Acceptance

- [ ] A new internal crate `flux-fleet` exists at layer **L3** and is registered in
      `flux-codegate`'s layer map, so `workspace_respects_layering` enforces its position rather
      than documenting it.
- [ ] The view types and the port move into it. `flux-tui` depends on `flux-fleet`, not the reverse,
      and `flux-cli`'s 692-line upward `impl FleetBoardSource` is deleted rather than relocated.
- [ ] `flux-cli` does not depend on `flux-tui` for any fleet type after the change. A test asserts
      the absent edge, because a re-added dependency is exactly the regression this story fixes.
- [ ] The extraction is behaviour-preserving: no fleet verb changes its output, and the existing
      `board_fleet_cli` tests pass unmodified. Any test that must change is evidence of a behaviour
      change and must be called out rather than edited quietly.
- [ ] The TUI reaches fleet state through the new port only, with no path back through the CLI
      binary.
- [ ] Full gate green in both workspaces: `scripts/release-full-gate.sh`.
