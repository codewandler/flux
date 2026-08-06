---
id: C-600
title: "Let the coordinator's operator transcript roll over instead of growing forever"
pillar: Core
epic: fleet-harness-throughput
status: ready
priority: 25
areas: [flux-cli, flux-tui, flux-flow]
note: "main's model calls already ignore retained history (current_turn: true); the durable session is an operator artifact that nothing bounds"
---

# Let the coordinator's operator transcript roll over

## Goal

Stop the attached Fleet coordinator's durable session from growing without bound, without pretending
the growth is a context problem. It is a *transcript* problem: the model never reads that history.

## Acceptance

- [ ] The coordinator's durable session has a bounded lifecycle — a documented rollover policy (per
      attach, per size, or per operator action) rather than one session that accumulates forever.
- [ ] Rolling over preserves prior transcripts as readable sessions; no operator history is deleted
      to make the current one small.
- [ ] `flux tui --fleet` attach time does not scale with total coordinator history.
- [ ] The story records why the coordinator is exempt from `compaction_attempt`, so the exemption is
      a decision rather than an accident.

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
