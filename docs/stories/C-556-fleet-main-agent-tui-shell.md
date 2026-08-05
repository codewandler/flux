---
id: C-556
title: "The fleet TUI is centered on the one main coordinator conversation"
pillar: Core
status: backlog
epic: board-fleet-tui
design: docs/designs/board-fleet-tui.md
areas: [flux-tui, flux-cli, flux-orchestrate]
depends_on: [C-570]
note: "follow-up UI — conversational main surface plus attention rail; CLI remains automation API"
---

# The fleet TUI is centered on the one main coordinator conversation

## Goal

Give a human one polished conversational surface for supervising the main coordinator while keeping
worker and decision attention visible but secondary.

## Acceptance

- [ ] The main coordinator transcript/composer owns focus; header shows fleet, active goals, wave and
      durable connection state.
- [ ] A responsive attention rail summarizes workers, open decisions, blocked work and red gates with
      keyboard/mouse navigation and a narrow-terminal fallback.
- [ ] Worker phase/attention comes from C-570's acknowledged report projection; raw model prose and
      host-observed tool activity may be shown separately but never impersonate worker status.
- [ ] Sending requirements, choosing a suggested decision and acknowledged follow-ups use the same
      typed fleet operations and display accepted/delivered/completed state.
- [ ] Restart reconstructs the view from durable events without terminal scraping.
- [ ] Accessibility, theme, snapshot and interaction tests cover narrow/wide layouts and busy workers.
- [ ] The TUI does not gain push/release/deploy or hidden board mutation authority.
