---
id: C-556
title: "The fleet TUI is centered on the one main coordinator conversation"
pillar: Core
status: in-progress
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

- [x] `flux tui` remains an explicitly labelled standalone chat; `flux tui --fleet[=ROOT]` validates
      one Fleet root, opens the main coordinator's isolated durable store and resumes its exact
      recorded session rather than whichever ordinary session happened to be latest.
- [x] The main coordinator transcript/composer owns focus; header shows the attachment mode, Fleet
      root, connection/revision, active goals and wave without allowing a stopped or invalid Fleet
      to masquerade as connected.
- [x] A responsive attention rail summarizes workers, open decisions, blocked work and red gates with
      keyboard navigation, mouse scrolling and a narrow-terminal fallback. `F2`, `/fleet` and
      `/board` open the operations surface; the ordinary composer keeps focus otherwise.
- [ ] Worker phase/attention comes from C-570's acknowledged report projection; raw model prose and
      host-observed tool activity may be shown separately but never impersonate worker status.
- [x] Sending requirements, choosing a suggested decision and acknowledged follow-ups use the same
      typed durable Fleet/Board bridge as the CLI and display accepted/delivered/completed or failed
      state. Observation alone is read-only; a decision changes only after an explicit confirmation.
- [x] Restart reconstructs the view from durable events without terminal scraping.
- [x] Accessibility, theme, snapshot and interaction tests cover narrow/wide layouts and busy workers.
- [x] The TUI does not gain push/release/deploy or hidden board mutation authority.

## Progress

- 2026-08-05 — promoted under C-582. The explicit `--fleet` launch is deliberate: repository
  detection may offer attachment later, but silently changing an ordinary chat's session store or
  coordinator authority would make the header lie.
- 2026-08-05 — shipped explicit exact-session attachment, the coordinator-first responsive shell,
  durable intake acknowledgement and the typed operations boundary. Failing-first CLI parsing,
  source fixtures, narrow/wide rendering snapshots, the full `flux-cli` and `flux-tui` suites, and
  warning-denying targeted Clippy cover the implementation.
