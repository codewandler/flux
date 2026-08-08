---
id: C-603
title: "Drive the implementation loop by goal checkpoints, not a fixed model-round ceiling"
pillar: Core
status: ready
priority: 12
epic: fleet-harness-throughput
design: docs/designs/agent-loop-harnesses.md
areas: [flux-flow, flux-cli, flux-tui]
depends_on: [C-570]
note: "max_rounds is an arbitrary guillotine that discards finished work; checkpoint on the assignment instead and report progress at each one"
---

# Drive the implementation loop by goal checkpoints

## Goal

Let a story worker run until its assignment is done or genuinely stuck, deciding that for itself at
explicit checkpoints, and report progress at each one — instead of being killed mid-commit by a fixed
provider-round ceiling that has no relationship to the work.

## Acceptance

- [ ] The implementation profile is an authored outer loop: run a bounded work segment, then a
      checkpoint that assesses the assignment and returns `continue`, `handoff` or `blocked`.
- [ ] The checkpoint decision is typed and inspectable — not prose the host has to parse loosely.
- [ ] At each checkpoint the worker emits a durable, acknowledged progress report (C-570) carrying at
      minimum: the assignment, what changed since the last checkpoint, and the decision.
- [ ] The coordinator can read those reports without deriving a per-worker filesystem path, and can
      summarize them to the operator — in the transcript or an authored surface pane.
- [ ] Failing first: a worker whose assignment needs more rounds than one segment allows completes
      across checkpoints and commits, where today it dies with `ai_segment exhausted its N model-round
      budget before completion`.
- [ ] An overall bound still exists (max checkpoints, wall-clock, or token budget) so a loop cannot
      run forever — the ceiling moves from "arbitrary" to "declared".

## Notes

- **What prompted it.** Fleet wave-281, `flux/C-562`, `max_rounds: 64`: the worker made 91 tool
  calls, wrote 878 lines across two files, and reached `git_stage` + `git_commit` — then died on
  `ai_segment exhausted its 64 model-round budget before completion`. The final staged batch carrying
  the commit never finalized, so the worktree held the whole implementation as uncommitted changes.
  Raising the number to 200 moves the guillotine; it does not remove it. The ceiling is not a property
  of the work.
- **A checkpoint loop is worthless unless the segment failure is CAUGHT.** Learned the hard way in
  wave-299, on the first dispatch of the authored checkpoint loop: the worker modified four files, added
  two new ones (including a test), then hit `ai_segment exhausted its 60 model-round budget`. An
  exhausted segment **raises** — it does not return a result — so the runtime error propagated straight
  out of `repeat` and the checkpoint never ran. The loop died in exactly the way it was written to
  prevent, with the work uncommitted. The work segment must sit inside `try … catch $err`, and the
  handler must bind the same `$answer` the checkpoint reads (pre-bound before the loop, so a caught
  segment leaves loop state valid rather than propagating an unbound symbol). The handler text must
  also tell the next segment to *resume from the worktree*, since finalized batches survive on disk and
  only staged ones are lost.
- **Instruct the segment to commit incrementally.** The recovery path only preserves what was
  committed or finalized. Every loss so far (wave-281, wave-299) was a worker holding a complete
  implementation in an uncommitted worktree because it planned to commit once at the end.
- **Design constraint that shapes the loop.** `ai_segment` is stateless by construction: with
  `current_turn: true`, `run_scoped_segment` sets `context.conversation = vec![one message]` —
  *"Authored segments never inherit retained conversation implicitly."* Its inputs are exactly
  `goal`, `tools`, `max_rounds`, `current_turn`, `max_tokens`, `max_history_bytes`; there is **no
  state input**, so a second segment cannot resume the first. An outer loop must therefore carry
  continuity forward explicitly — the checkpoint's summary becomes part of the next segment's `goal`.
  That is a feature for context hygiene (each segment starts clean) but it means the checkpoint must
  produce a genuinely sufficient handover, and the story should treat that summary as the loop's real
  contract.
- **Flux-Lang already has the control flow.** Authored loops support `repeat N until $x`,
  `when`/`else`, `match`/`case`, `$bind` and `fmt`, and the loop file is where a profile's behavior is
  meant to live — this is exactly the "authored Flux-Lang owns control flow, the model owns judgment"
  split the architecture is built on. No new language feature is required for the loop itself.
- **The reporting half is C-570** — *"SpawnActivity is host telemetry; add durable child-authored
  reports, acknowledgement and a resumable safe-checkpoint yield"*, whose phases (`active`,
  `waiting`, `candidate_ready`, `handoff_ready`, `budget_warning`) already match the checkpoint
  decisions above. Decision 0014 §3 requires such an operation and it does not exist; workers have no
  `fleet.*` operation at all today.
- **Transport to the coordinator is [C-602](C-602-fleet-workers-report-activity-back-to-the-coordinator.md)**,
  which must not assume shared local storage because a worker is meant to become containerisable.
- Together these three close the loop the operator actually wants: the worker decides when it is
  done, says so at each checkpoint, and the coordinator can answer "how is it going?" without
  interrogating anything.

## Evidence — wave-602

The checkpoint loop is in place in the roadmap's `story-implementation.flux` and it works: all three
surviving workers hit the ceiling and none of them lost work.

    adaptive model-call budget exhausted before `explore` (60/60 calls used in this logical run)
    ai_segment exhausted its 60 model-round budget before completion

`wave-602-worker-1` hit it twice, `wave-602-worker-2` and `wave-472-worker-9` once each. Both
wave-602 workers went on to commit cleanly, which is precisely the outcome this story exists to
produce and did not happen before checkpointing.

What the measurement adds is the **cost of the restart**, which the current design does not address.
The segment is stateless by construction and the carry is prose, so a resumed segment re-derives its
bearings by re-reading the tree. `wave-602-worker-1`'s phase split across its whole assignment:

| phase | steps |
|---|---|
| `gather` | 166 |
| `execute` | 62 |

2.7 gather steps per execute step, with 88 `read` and 63 `grep` calls, for a change of 946 lines
across 7 files. The two exhausted segments consumed 994 s and 790 s of wall clock before being cut —
about 73% of that worker's 40-minute run spent in segments that ended without returning.

The work survives; the *context* does not. That suggests the checkpoint's typed decision (acceptance
item 2) should carry enough structure for the next segment to resume from what was already
established — the files that matter, the design read, the failing test — rather than a prose summary
that leaves rediscovery as the next segment's first job.
