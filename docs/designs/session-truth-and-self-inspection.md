# Session truth and self-inspection

**Status:** planned (C-589) · **Pillar:** Core

## Problem

Session `s_2013` completed a real delegated mutation and then lost the ability to explain it. The
durable event stream records `task` calls, child sessions `s_2014`, `s_2015` and `s_2016`, the
successful `flux plugin uninstall babelforce-manager` result, and post-change verification. Later
turns received only conversational messages, inferred that no direct shell schema meant no command
could have run, and repeatedly replaced the verified history with the false claim that the earlier
execution was fabricated.

The data is present. The missing product boundary is a bounded, typed way for both operators and
agents to query it, plus a small host-derived execution receipt that survives the next turn,
compaction, continuation and resume.

## One query service, several projections

`SessionQueryService` is a read-only projection over the existing event store. CLI and operations
consume it; neither parses SQLite or reconstructs causal links independently.

The list query accepts an exact session id in addition to the existing content, touched-file and
time predicates. The detail query accepts one exact id (including `current`/`last` where the caller
has that authority), optional turn selection, an explicit include set, a result-byte ceiling and a
cursor. It returns:

- session/turn identity, model and outcome;
- redacted user/assistant messages when requested;
- accepted plans and action batches;
- operation name, status, denial/error and durable event sequence;
- correlated child session/role, child tool-call count and child result status;
- host-recorded usage totals and typed omission metadata.

Raw reasoning, system/developer prompts, credentials and unredacted operation bodies are never an
inspect field. Inputs/results use the same durable redacted views already eligible for export and
replay. Detail is bounded by construction; `complete`, `omitted` and `next_cursor` prevent a clipped
response from masquerading as the full record.

CLI projections are `flux sessions --id s_2013 [--json]` for exact selection and
`flux session inspect s_2013 [--turn N] [--children] [--json]` for causal detail. The singular
inspection command complements rather than overloads the newest-first plural listing.

Agent projections are `session.list` and `session.inspect` operations over the same request/response
types. `session.inspect` defaults to the caller's own session and requires an explicit permission
subject for another session. Intent routing surfaces the family for questions about the current
conversation, prior actions, sub-agents, transcript, session ids, or “what did you run?”. This is an
operation family, not implicit prompt injection of arbitrary old transcripts.

## Continuation receipt and evidence rules

At turn settlement the host records a bounded `TurnExecutionReceipt/v1`: turn/session identity,
accepted batch ids, each action's operation/status/effect class, correlated child ids/roles, verified
result state, usage summary and omission metadata. It contains no raw command output. The next turn
receives the latest receipt as host facts, and continuation/resume reconstructs it from events.

The system contract teaches the capability boundary explicitly: an agent may delegate with `task`;
the child can receive a narrower but different operation set; absence of a direct parent operation
does not prove that no child executed it. A model may qualify that evidence is not in its current
chat context, but it must not label a previously host-verified execution “fabricated” without first
checking the receipt or `session.inspect`. Durable execution evidence outranks inference from the
currently surfaced operation schemas.

## Bounds and ownership

- Event records remain the only source of truth; no transcript cache or second state machine.
- The query service is read-only and safe for concurrent readers.
- The operation family is unavailable to Fleet workhorses unless their admitted loop/capability
  policy grants it; inspecting history never grants authority to resume or mutate it.
- The receipt is execution provenance, not conversation memory and not Board/Fleet runtime state.
- Resource totals reuse canonical `CallUsage`/C-575 receipts and never sum parent rollups plus child
  usage twice.

## Delivery order

1. C-590 adds the shared query/detail projection, exact-id CLI selection and agent operations.
2. C-591 adds the continuation receipt, capability-boundary instruction and the hermetic `s_2013`
   regression.
