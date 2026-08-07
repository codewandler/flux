---
id: C-618
title: "fleet resume must not replay the coordinator inbox into a story worker"
pillar: "Core"
status: ready
priority: 6
epic: fleet-harness-throughput
areas: [flux-cli]
---

# fleet resume must not replay the coordinator inbox into a story worker

## Goal

Keep a resumed agent's assignment its own. `fleet resume <worker>` currently hands the resumed agent
the **coordinator's** pending inbox, so a story worker is asked to answer messages addressed to `main`.

## Acceptance

- [x] A resumed story worker's prompt contains its own assignment and its own pending items only.
- [x] Coordinator-addressed intakes stay addressed to the coordinator and are not consumed by any other
      agent's resume.
- [x] Failing first, a test resumes a worker while coordinator intakes are pending and asserts none of
      them appear in the worker's assignment.
- [x] An intake is delivered at most once; a resume that does not target the coordinator must not
      consume or acknowledge coordinator items.

## Notes

**Observed directly.** With eight coordinator intakes pending, `flux fleet resume wave-308-worker-1`
built this assignment for a *story worker*:

```
Current assignment:
Resume from the durable manifest and process these accepted …
- intake-78: Autopilot tick. Report only: current revision, active workers …
- intake-79 … intake-85   (the same, once per tick)
- message-49: Finish only these C-560 items in your existing session …
- task-3: Read-only smoke test: inspect the native board …
```

None of those belong to `wave-308-worker-1`, whose assignment is `flux/C-562`. Three separate problems
compound:

1. **Wrong addressee.** Coordinator intakes reached a worker. The worker contract explicitly forbids
   coordinating Fleet, so it is being handed work it is instructed to refuse.
2. **Context pollution.** This is the concern that motivated the evidence-scope work: a worker that
   should see only its assignment is handed unrelated operator traffic, and a second worker resumed
   later would see the same items again.
3. **No delivery bound.** The items were not consumed, so every subsequent resume replays them. Eight
   accumulated in eight minutes of a one-minute reporting cadence.

**Interim mitigation, not a fix.** The autopilot no longer requests a report through `fleet ingest`
every tick; it logs a deterministic status line and asks the coordinator to narrate only when the
wave-state fingerprint changes. That reduces the queue growth that made the bleed obvious, but the
bleed itself is unaffected — any pending coordinator intake still lands in the next resumed worker.

**Related bound worth adding at the same time.** A stalled wave was resumed once per tick, each attempt
costing a full worker turn. The autopilot now caps attempts and parks the wave, but Fleet should also
refuse to resume an agent that a previous resume has not finished.

- Related: [C-616](C-616-a-story-worker-authors-its-own-handoff-instead-of-a-third-party-transcribing-it.md)
  — the same boundary from the other direction: a worker cannot report *out*, and here it receives what
  it should never see.
