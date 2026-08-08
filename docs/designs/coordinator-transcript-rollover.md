# Design record: the coordinator's transcript has a lifecycle

**Status:** accepted · **Story:** [C-600](../stories/C-600-the-coordinator-session-grows-forever-for-a-model-that-never-reads-it.md) ·
**Epic:** [fleet-harness-throughput](fleet-harness-throughput.md)

## What was wrong

The attached Fleet coordinator recorded one durable session and appended to it forever. Observed at
8.2 MB after ~75 intake items and still growing, with `flux tui --fleet` rebuilding the whole thing
through `ChatState::project_session` on every attach.

The tempting reading — "the coordinator's context is too big, compaction will handle it" — is wrong
on both halves.

## Why the coordinator is exempt from `compaction_attempt`

This is a decision, not an oversight.

- **The model never reads that history.** `.flux/fleet/loops/main-coordinator.flux` sets
  `current_turn: true`, and `EngineLoopHost::run_scoped_segment` then replaces the conversation with
  a single message: instructions + the current request + the typed Board/Fleet catalogue. Turn 75
  does not see turn 1. Coordinator status comes from `board.*`/`fleet.*` reads, which is the intended
  design.
- **Compaction is structurally unreachable anyway.** `FlowEngine::compaction_attempt` runs only for
  `TurnProgram::Adaptive`; an authored coordinator loop is `TurnProgram::Authored`.
- **It should stay unreachable.** Compaction rewrites history to relieve *context* pressure. The
  coordinator has no context pressure — it has an *operator artifact* with no lifecycle. Applying
  compaction here would summarize away the only readable record of what the operator and the
  coordinator actually said, to solve a problem the model does not have.

So the bound is a rollover, not a rewrite: growth is capped by retiring a segment, and every retired
segment stays intact and readable.

## The policy

Rollover is **per size, evaluated at resume**.

- A segment may hold `COORDINATOR_TRANSCRIPT_ROLLOVER_EVENTS` (1000) durable events. At the next
  resume — TUI attach *or* headless drive — a segment at or over the ceiling is not resumed; the
  turn mints a fresh segment in the same store.
- The ceiling is counted in **events**, because events are exactly what an attach pays for:
  `ChatState::project_session` is proportional to the segment's event count, not to the store's
  total history. The check itself reads only the stream head from the session registry
  (`EventStore::head_seq`), never the events, so the bound is not paid for by the thing it bounds.
- Both entry points apply it. `prepare_fleet_tui` decides the segment before the surface projects
  anything; `main_turn_spec` applies the same ceiling on the headless path, which is where the 8.2 MB
  session actually grew, with nobody attached.

## Identity across a roll

Fleet deliberately resumes the *recorded* main session and flux-roadmap decision 0014 treats that
durable identity as meaningful, so a roll has to say what survives it.

- The **agent** identity is unchanged: still `main`, still one coordinator, still one store at
  `<git-dir>/flux-fleet/sessions/main`.
- The **transcript** identity is per segment. `state.json` records the current segment in
  `main_agent.session` and pushes the segment it replaced onto `main_agent.session_history`
  (oldest first). `fleet.tui.attached` reports `rolled_from`, so a roll is a journalled event rather
  than a silent substitution.
- `attach_session` still refuses an id that disagrees with a recorded segment *under* the ceiling —
  the original conflict guard is intact. The refusal is waived only for a segment the resume path has
  already declined to resume.
- Recording the same id again is a resume, not a roll, and pushes nothing.

## Nothing is deleted

Every segment — current and retired — is minted in the **same store**, so
`flux sessions --store <git-dir>/flux-fleet/sessions/main` and the TUI session picker continue to
list all of them, and a retired transcript can still be read end to end. The bound applies to the
live segment's *size*, never to the operator's history.

The only bounded-with-loss structure is the `session_history` **index** in `state.json`
(`COORDINATOR_TRANSCRIPT_HISTORY` = 50 ids). A field that only grows is how one Fleet state file
reached 12.9 MB; dropping the oldest *pointer* costs nothing because the store, not `state.json`, is
the durable record.

## Failure posture

The ceiling check fails **open**: an absent or unreadable store keeps the recorded identity rather
than starting a second transcript. Forking the coordinator's durable identity on a transient IO
error would be worse than an oversized transcript.

## Alternatives rejected

- **Compact the coordinator session.** Rewrites the operator's record for a model that never reads
  it; see above.
- **Truncate or prune old events.** Violates the requirement that no operator history is deleted to
  make the current transcript small.
- **A new store per roll.** Would bound attach cost too, but breaks `flux sessions --store` and the
  TUI picker as the single place a coordinator's history is enumerable.
- **Roll on every attach.** Cheap to implement, but shreds a working session into one segment per
  operator restart and discards the resume behavior decision 0014 asks for.
