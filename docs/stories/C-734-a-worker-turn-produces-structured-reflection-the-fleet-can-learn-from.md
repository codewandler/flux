---
id: C-734
title: "A worker turn produces structured reflection the fleet can learn from"
pillar: "Core"
status: backlog
priority: 3
epic: delivery-is-verified
areas: [flux-orchestrate]
note: "C-587 built the review half of its story and deliberately ticked no acceptance box, because every item is a compound review-and-reflection claim and only review is true. Outstanding: the AgentReflection/v1 document, the bounded worker-transcript packet it is derived from, the central improvement projection, and flux fleet insights. Review judges one candidate; reflection is how the fleet gets better at producing them"
---

# A worker turn produces structured reflection the fleet can learn from

## Goal

C-587 built the **review** half of its story and deliberately ticked no acceptance box, because every
item was a compound review-and-reflection claim and only review was true. This story is the other
half.

Review judges one candidate. Reflection is how the fleet gets better at producing them. Today a
worker turn ends and everything it learned — what it tried, what the gate rejected, which instruction
was ambiguous, how long it spent where — is discarded with the session. The fleet has run hundreds of
turns and has learned nothing from any of them, structurally: there is no artifact to learn from.

Outstanding: the `AgentReflection/v1` document, the bounded worker-transcript packet it is derived
from, the central improvement projection, and `flux fleet insights`.

## Acceptance

- [ ] An `AgentReflection/v1` document is produced at the end of a worker turn, with a stated schema.
      It is derived from a **bounded** transcript packet — bounded is load-bearing, since an
      unbounded one puts a whole session into durable state, and `state.json` already grows without
      limit.
- [ ] Producing it cannot fail the turn. A worker whose reflection cannot be written still delivers
      its work, with the failure reported.
- [ ] The reflection is derived from what the turn actually did — its commits, its gate results, its
      rework requests — and not only from what the model says about itself. A self-report with no
      artifact behind it is the failure mode this repository names most often.
- [ ] A central improvement projection aggregates reflections across turns, and `flux fleet insights`
      reads it. A projection nothing queries is a log, not a projection.
- [ ] The projection answers at least one question that changes behaviour — for example which
      instruction most often precedes a rework — and the story records the answer it gave on real
      data rather than only that the query runs.
- [ ] Reflection is fenced from the shared ledgers exactly as worker output is, so two concurrent
      turns cannot make a wave unintegrable by both writing one file.
- [ ] Failing-first test for the schema and the boundedness limit.
- [ ] Full gate green: `scripts/release-full-gate.sh`.
