---
id: C-151
title: Show relative time in the session picker
pillar: Core
status: backlog
epic: tui-polish-round-2
design:
note: "rows render `id · N msg · model` only (rendering.rs:255-258) although SessionSummary already carries created_at_ms/updated_at_ms (flux-events/src/store/mod.rs:128-129) — a formatting change, no new data"
---

# Show relative time in the session picker

## Goal
Resuming the right session is a time question ("the one from this morning"), but the picker row is
`● <id>  · <n> msg · <model>` (`rendering.rs:255-258`). The data is already in hand:
`flux_events::SessionSummary` carries `created_at_ms` and `updated_at_ms`
(`crates/flux-events/src/store/mod.rs:128-129`) and the picker holds whole summaries
(`state.rs:50`). Render a relative "2h ago" so the list is navigable without resuming to check.

## Acceptance
- [ ] Each session row shows a compact relative age derived from `updated_at_ms` — failing-first
      test extending `session_picker_is_dense_and_marks_the_active_session` (`lib.rs:5553`).
- [ ] The row stays one line and stays truncated to the overlay width (`rendering.rs:266`); the
      active-session marker and the `n/m` counter are unchanged.
- [ ] Formatting is deterministic under test (no wall-clock dependence in the assertion).

## Progress
- (not started)

## Notes
- Deliberately scoped to the picker. Per-entry transcript timestamps are a separate, larger change:
  `ChatState` has `turn_start`/`last_elapsed` (`state.rs:16,36`) but `Entry` carries no timestamp,
  so that half needs a new field and a projection decision.
