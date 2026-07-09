---
id: A-15
title: Phase-aware surface — loop.phase spinner labels, brief render, compact gather render
pillar: Agent
status: done
priority:
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: CLI + TUI parity; observations already pass drain_event unfiltered so no plumbing change — pure rendering
---

# Phase-aware surface

## Goal
Make the phases visible: the spinner reads "orienting… / planning… / revising…" (`loop.phase`
observations emitted at `plan()` entry), the brief renders the moment it's accepted
(`flow.brief` → `◆ goal: …` + dim needs list), gather plans render as a compact one-liner
(`gathering · read Cargo.toml, src/lib.rs · grep "LoopHost"`), and execution plans keep the full
tree + risk badge.

## Acceptance
- [x] Host emits `loop.phase {phase}` at `plan()` entry and `flow.brief` on brief acceptance
      (A-14, verified still holding). `{phase, round}` (the `round` field) and `flow.plan` gaining
      its own `phase` field did **not** land — that's `flux-flow` plumbing, out of scope for this
      story (A-16 was mid-edit there concurrently); the surface derives phase/gather-mode entirely
      from its own tracked state (`loop.phase` + `flow.brief` order) instead, so the UX bullet is
      met without needing those extra fields. Flagged as a residual, see Notes.
- [x] `CliSink` (`crates/flux-cli/src/main.rs`): phase-labeled spinner (`phase_spinner_label`),
      brief render (`brief_lines`/`render_brief`), compact gather render
      (`gather_compact_line`/`render_gather_compact`), full tree unchanged for execute-phase plans
      (`render_plan`). Tests: `loop_phase_observations_drive_the_phase_labeled_spinner`,
      `flow_brief_observation_marks_gather_mode_and_formats_goal_and_needs`,
      `gather_plan_renders_as_a_compact_one_liner_not_the_full_tree`,
      `flow_plan_dispatches_compact_or_full_by_gather_mode`.
- [x] flux-tui renders the same observations (parity pass): `ChatState::record_loop_phase` +
      `loop_phase_label`, `Entry::Brief`, `Entry::GatherPlan`/`plan::render_compact`, new
      `UiEvent::Phase`/`UiEvent::Brief` forwarded by `ChannelSink::observation`. Tests:
      `loop_phase_observation_drives_the_phase_labeled_spinner`,
      `channel_sink_forwards_phase_and_brief_observations`, `brief_entry_renders_goal_and_needs`,
      `gather_plan_entry_renders_compact_not_full_tree`.
- [x] Machinery filtering unchanged: verified `drain_event` (`engine.rs:945-956`) only matches
      `SinkEvent::ToolCall`/`ToolResult` for the `--show-loop` gate — `Observation` events (which
      carry `loop.phase`/`flow.brief`/`flow.plan`) always pass through; no change needed or made.
- [x] Gate green (package-scoped `-p flux-cli -p flux-tui`; see Progress).

## Progress
- 2026-07-03: Surfaced A-14's `loop.phase`/`flow.brief` observations in both `CliSink` and
  flux-tui. Spinner label is phase-derived: "orienting…" (orient) / "gathering…" (gather) /
  "planning…" (execute, first round this turn) / "revising…" (execute, 2nd+ round this turn) —
  the last is a per-turn counter over `loop.phase` observations already reaching the sink, no new
  flux-flow signal needed (`CliSink::execute_rounds` / `ChatState::execute_rounds`). Gather-vs-full
  plan rendering is derived the same cheap way: `gather_mode` flips true on a `gather`-phase
  `loop.phase` or any `flow.brief` (a brief only ever accompanies a `gather: true` plan) and false
  on `orient`/`execute` — `flow.plan` itself carries no `gather` flag today, so this state machine
  is the surface-side stand-in. One known gap: a `gather: true` plan whose model omitted a usable
  `brief` during the very first `orient` round renders full instead of compact (no signal
  distinguishes it from orient emitting the full plan directly) — narrow, tolerant-parsing edge
  case, documented rather than worked around (would need a `flow.plan.gather` field from
  flux-flow, out of scope here). CLI gate: `cargo build/test/clippy -D warnings -p flux-cli`
  green (67 tests, incl. 4 new). TUI gate: same `-p flux-tui` green (29 tests, incl. 5 new).
  `cargo fmt -p flux-cli -p flux-tui` clean. Did not run the full-workspace gate (flux-flow was
  mid-edit under a concurrent story, A-16) — only the two owned crates were built/tested/linted,
  per the story's scoping.

## Notes
- Depends on A-14 (the observations exist). `drain_event` filters only machinery
  ToolCall/ToolResult (`engine.rs:939-950`) — observations flow already.
- Revision rendering (`✗ step 4/9 — revising…`, ✓-done prefix marks) lands with A-17. This story's
  "revising…" is the *spinner* label only (a plan-level cue while the model is composing the next
  round), not the halt-aware per-step marking A-17 will add to the completed plan render.
- Residual/follow-up (not filed as a separate story — small enough to fold into A-16/A-17's own
  polish, or pick up ad hoc): if `flux-flow` ever adds `round` to `loop.phase` or a `gather` flag
  to `flow.plan`, the CLI/TUI state machines here can drop their phase-transition inference in
  favor of reading it directly — flagged in the Acceptance above rather than built against a
  moving concurrent target.
- **Closed by A-17 (2026-07-02):** `flow.plan` now carries `gather`/`phase`/`resumed` fields,
  computed host-side from the plan's own `settled` signal (not surface-side inference); CLI/TUI's
  `flow.plan` dispatch prefers the direct field (falling back to the `loop.phase`/`flow.brief`
  state machine only when it's absent), closing the exact gap noted above.
