# Design — Task-agent backends and CLI harness adapters

**Status:** proposed follow-up · **Stories:** [C-552](../stories/C-552-task-agent-backend-contract.md),
[C-553](../stories/C-553-cli-agent-harness-adapters.md)

## Why

Fleet membership and task execution are separate concerns. The V1 fleet can admit and coordinate
native local Flux sub-agents, but a task may eventually run through a different local harness. A
generic backend must let the coordinator start, steer, cancel, resume and inspect an agent without
teaching fleet scheduling about Codex, Claude, Hermes or Pi.

## Boundary

`TaskAgentBackend` owns process/session lifecycle, typed capabilities and acknowledged steering. A
fleet admission binds one backend instance plus its declared mode/fences to a worker record. Backend
receipts carry exact session and terminal evidence; stdout prose is never the control protocol.

The first adapter wave covers installed local CLI harnesses for Codex, Claude, Hermes and Pi. It
uses argv-only process launch, closed environment forwarding, version/capability discovery, durable
session identifiers and cancellation. Provider-specific prompt/config material stays in adapter
configuration, not in fleet core.

## Non-goals

Remote A2A membership, containers, automatic publication and UI are separate epics.
