---
id: C-616
title: "A story worker authors its own handoff instead of a third party transcribing it"
pillar: "Core"
status: ready
priority: 14
epic: fleet-harness-throughput
areas: [flux-cli]
---

# A story worker authors its own handoff instead of a third party transcribing it

## Goal

Let the agent that did the work record the handoff, because it is the only party holding the evidence
first-hand. Today a story worker has no `fleet.*` operation, so its handoff must be transcribed by a
coordinator or an operator from prose — which is lossy, unfalsifiable, and does not scale.

## Acceptance

- [ ] The `implementation` task kind carries a bounded native `fleet.handoff` operation, admitted the
      same way `integration` receives `fleet.integrate`/`fleet.status`.
- [ ] The operation is scoped to the caller's own wave and board item; a worker cannot hand off
      another worker's story, another wave, or an item it was not assigned.
- [ ] It does not weaken the evidence gate — failing-before/passing-after is still re-run by the host,
      never accepted on the worker's word.
- [ ] The recorded write set comes from what the worker actually wrote, not from a diff computed
      afterwards by someone else.
- [ ] Failing first, a test proves a worker calling `fleet.handoff` for a foreign wave or item is
      refused at the operation.
- [ ] The story-worker contract instructs the worker to *call* the handoff, replacing "return a durable
      handoff" — which today describes prose that nothing consumes mechanically.

## Notes

**Observed directly on wave-302**, the first wave to produce a clean commit. `flux fleet run` reported
`wave-302 agents completed; handoffs required` and stopped. `story["handoff"]` is assigned in exactly
one place (`fleet_handoff`), so nothing in the run path can record it. The handoff was therefore
assembled by hand, and every weakness was visible in that one exercise:

- the write set was read out of `git diff --name-only` **after the fact** rather than from the worker's
  observed writes — if the two ever disagree, the recorded evidence is fiction;
- `--failing-before` was asserted without any failing run having been observed. It was true only
  because the gate re-runs the argv at the pinned base;
- `--worker`/`--session` had to be recovered from `state.json` after the command refused with
  `handoff worker identity is missing or ambiguous` — the caller had to reconstruct facts the worker
  already knew.

**Why delegating this is safe.** `fleet_handoff` already re-runs the validation argv itself: once in
the integration worktree at the pinned base (which must fail or match zero tests, via `ran_no_tests`)
and once in the story worktree at the returned commit (which must pass). A worker-supplied claim is
therefore *checked*, not trusted, so moving authorship to the worker adds no new trust. The present
split has the party **without** the evidence making the assertion and the host verifying it; the same
verification works unchanged with the party who has it.

**Precedent to copy, not invent.** `task_kind_native_operations` already grants one task kind a
task-scoped native Fleet operation set, and `native_fleet_integrator_operation_ceiling` already
demonstrates a narrow ceiling (assemble and read status; no apply, push, dispatch or Board
transition). `fleet.handoff` for `implementation` is that same shape, one stage earlier in the
pipeline.

- Related: **C-570** — durable child-authored reports and acknowledgement; a handoff is the terminal
  case of the same channel, so the two should share a transport rather than grow separate ones.
- Related: [C-603](C-603-implementation-loop-checkpoints-against-its-goal.md) — the checkpoint that
  decides the assignment is complete is the natural place to emit the handoff.
