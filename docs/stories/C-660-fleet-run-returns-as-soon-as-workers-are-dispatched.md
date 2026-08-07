---
id: C-660
title: "fleet run returns as soon as workers are dispatched"
pillar: "Core"
status: ready
priority: 7
areas: [flux-cli]
epic: fleet-harness-throughput
---

# fleet run returns as soon as workers are dispatched

## Goal

`flux fleet run` blocks until every dispatched worker's turn finishes. A wave of story workers is
minutes to hours of model time, so the operator's terminal — or the coordinator's own turn — is held
for the whole wave. That makes dispatch and observation the same act, when they are not: dispatch is
a state mutation that either happened or did not, and observation is a projection you should be able
to ask for whenever you like.

The practical damage: nothing else can be scheduled while a wave runs, a cancelled terminal looks
like a cancelled wave, and the coordinator cannot dispatch and then answer an operator question.

## Acceptance

- [ ] `fleet run` returns once workers are dispatched and their assignment is durably recorded,
      reporting the wave id and the workers started.
- [ ] Turn completion is observed through `fleet status`/`fleet events`, not by holding the caller.
- [ ] A caller that goes away — closed terminal, cancelled turn — does not stop or orphan a
      dispatched worker; liveness stays the recorded supervisor, not the client.
- [ ] `--wait` remains available for scripts that genuinely want the old behaviour, with a bound.
- [ ] Failing first, a test proves dispatch returns before a worker's turn ends and that the wave
      still reaches a terminal state afterwards.
