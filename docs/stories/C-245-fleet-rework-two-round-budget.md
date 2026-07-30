---
id: C-245
title: "`fleet.rework` — the 2-round budget as a host rule, so a third round parks instead of dispatching"
pillar: Core
status: backlog
epic: fleet-loop
design: docs/designs/fleet-loop.md
areas: [flux-tools, flux-capabilities]
note: "F8 — rework must reach the SAME worker (contextId continuity already exists); the budget is enforced by the op, not asked for in a prompt"
---

# `fleet.rework` — the 2-round budget as a host rule, so a third round parks instead of dispatching

## Goal
Rework has to reach the **same** worker — a fresh worker re-reads the whole problem and loses the
context that made the findings actionable. A2A session continuity keyed on `contextId` already
provides this (`find_or_mint_session`, `crates/flux-server/src/a2a.rs:88`), and the board already
holds the item's `context_id`; nothing ties them together.

Add `fleet.rework`: re-dispatch findings to the same worker session via the board's `context_id`.
And make the 2-round budget a **host rule**: round 3 returns a `park` signal instead of dispatching.
A budget in a prompt is a suggestion; a budget in the op is a fact.

## Acceptance
- [ ] **Failing-first test**: rework reaches the same worker twice — proved via that worker's session
      log, not via its self-report — and a third call yields `park`, not a third dispatch.
- [ ] The park path sets the item's state and records *what* is unresolved, so a parked item is a
      successful outcome with a note rather than a silent drop.
- [ ] The budget cannot be laundered: `attempts` is authoritative and the `Blocked→Ready` hole is
      already closed by F2 (C-240), so cycling through `blocked` does not buy a fourth round. Pin
      this interaction with a test.
- [ ] Findings are passed as structured input, not concatenated prose, and carry the `path:line` or
      command output they were derived from.
- [ ] Accurate `effects`/`access`/`intents` and concrete `permission_subjects`.
- [ ] Standard gate green in both workspaces.

## Notes
- Depends on **F6 (C-243)** for a worker whose session persists, and on **F2 (C-240)** for the
  `attempts` fix that makes the budget uncheatable.
- The rule this encodes, from the `track` contract: rework carries findings with evidence and does
  **not** prescribe the fix — the worker has the context; the coordinator has the standard.
