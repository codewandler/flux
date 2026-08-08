---
id: C-600
title: "Let the coordinator's operator transcript roll over instead of growing forever"
pillar: Core
epic: fleet-harness-throughput
status: done
design: docs/designs/coordinator-transcript-rollover.md
areas: [flux-cli, flux-tui, flux-flow]
note: "main's model calls already ignore retained history (current_turn: true); the durable session is an operator artifact that nothing bounds"
---

# Let the coordinator's operator transcript roll over

## Goal

Stop the attached Fleet coordinator's durable session from growing without bound, without pretending
the growth is a context problem. It is a *transcript* problem: the model never reads that history.

## Acceptance

- [x] The coordinator's durable session has a bounded lifecycle — a **per-size rollover evaluated at
      resume**: a segment holding `COORDINATOR_TRANSCRIPT_ROLLOVER_EVENTS` (1000) durable events is
      not resumed, and the next resume mints a fresh segment in the same store. Documented in
      [the design record](../designs/coordinator-transcript-rollover.md) and applied on both entry
      points — `prepare_fleet_tui` (attach) and `main_turn_spec` (headless drive), the path that
      actually grew the 8.2 MB session with nobody watching.
- [x] Rolling over preserves prior transcripts as readable sessions. Every segment is minted in the
      same store, so `flux sessions --store <git-dir>/flux-fleet/sessions/main` and the TUI picker
      still enumerate all of them; `main_agent.session_history` indexes the retired ids and
      `fleet.tui.attached` reports `rolled_from`. Failing first,
      `a_full_coordinator_transcript_rolls_over_at_the_next_attach`
      (`crates/flux-cli/src/board_fleet_cmd.rs`) asserts the retired segment still loads intact after
      the roll, and `a_replaced_coordinator_segment_is_retained_not_forgotten` asserts a replaced id
      is retained rather than overwritten (and that re-recording the same id is a resume, not a
      roll).
- [x] `flux tui --fleet` attach time does not scale with total coordinator history: the attach
      projects one segment, and the ceiling check itself reads only the stream head
      (`EventStore::head_seq`), never the events. Same test: a full segment yields
      `FleetTuiLaunch { session: None, .. }` instead of a 1000-event projection, while a segment
      under the ceiling is still resumed — the durable identity Fleet deliberately keeps.
- [x] The exemption from `compaction_attempt` is recorded as a decision, in
      [the design record](../designs/coordinator-transcript-rollover.md) and on
      `COORDINATOR_TRANSCRIPT_ROLLOVER_EVENTS` itself: the model never reads this history
      (`current_turn: true`), compaction only runs for `TurnProgram::Adaptive` so it can never fire
      for an authored loop, and it *should* not — compaction would rewrite the operator's only
      readable record to relieve a context pressure that does not exist.

## Progress

- Landed in `crates/flux-cli/src/board_fleet_cmd.rs`: `coordinator_resume_session` /
  `coordinator_transcript_is_full` / `coordinator_transcript_events`, the
  `MainAgentState::session_history` index with `record_session`, and the two ceilings
  (`COORDINATOR_TRANSCRIPT_ROLLOVER_EVENTS` = 1000 events per segment,
  `COORDINATOR_TRANSCRIPT_HISTORY` = 50 indexed ids).
- The ceiling check **fails open**: an absent or unreadable store keeps the recorded identity. A
  transient IO error must never fork the coordinator's durable identity into a second transcript.
- `attach_session`'s conflict guard is intact — an id that disagrees with a segment *under* the
  ceiling is still refused. The refusal is waived only where `prepare_fleet_tui` has already declined
  to resume.
- Only the `state.json` *index* is lossy at 50 ids; the store keeps every transcript. A field that
  only grows is how one Fleet state file reached 12.9 MB.

## Notes

- **The model does not read this history.** `.flux/fleet/loops/main-coordinator.flux` sets
  `current_turn: true`, and `EngineLoopHost::run_scoped_segment` then does:

  ```rust
  // Authored segments never inherit retained conversation implicitly. `current_turn` above
  // copies only the latest request into this fresh one-message segment.
  context.conversation = vec![Message::user_text(&effective_goal)];
  ```

  So every coordinator turn is instructions + the current request + the typed Board/Fleet catalogue.
  Turn 75 does not see turn 1. Status comes from `board.*`/`fleet.*` reads, which is the intended
  design (`main.md`: "Use acknowledged Fleet messages and typed status/progress").
- **Compaction is structurally unreachable here.** `FlowEngine::compaction_attempt` runs only for
  `TurnProgram::Adaptive`; an authored coordinator loop is `TurnProgram::Authored`. So the usual
  answer ("it'll compact") never fires, and nothing else bounds the file.
- Observed 2026-08-06: `.git/flux-fleet/sessions/main/events.db` at **8.2 MB** after ~75 intake
  items, still growing. The TUI reconstructs the whole transcript through
  `ChatState::project_session` on every attach.
- Consequence to weigh in the design: Fleet deliberately resumes the *recorded* main session
  (`flux tui --fleet` "does not mint a new Fleet-main session"), and decision 0014 treats that
  durable identity as meaningful. A rollover policy has to say what identity means across a roll —
  which is why this is a story with a decision, not a bug fix.
- Related: [C-599](C-599-fleet-work-is-unobservable-while-it-runs.md) — both are about the
  coordinator surface being an operator artifact with no lifecycle of its own.
