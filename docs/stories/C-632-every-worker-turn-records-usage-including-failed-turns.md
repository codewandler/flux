---
id: C-632
title: "Every worker turn records usage, including failed turns"
pillar: "Core"
status: backlog
epic: fleet-harness-throughput
areas: [flux-events]
note: "no recent worker turn carries rounds or tokens; you cannot see how close a worker was to its round budget before it died; activity.ndjson has no timestamps, no turn id, and a proven torn write at line 556"
---

# Every worker turn records usage, including failed turns

## Goal

No recent worker turn records how much budget it used: receipts carry loop bindings and stream
budgets but no rounds-consumed or token counts, failed turns carry nothing at all, and
`activity.ndjson` — the only surviving record of a failed worker — has no timestamps, no turn ids,
no sequence numbers, and a proven torn write from concurrent appenders. Budget-pressure findings
(the guillotine class) are invisible until they kill something.

## Acceptance

- [ ] Every agent.turn.completed and agent.turn.failed event carries usage: rounds consumed vs limit and token counts.
- [ ] activity records carry ts, turn id and seq, and concurrent appends cannot tear (locked append or single writer).
- [ ] The TUI/status surfaces remaining round budget for a live worker.


## Comments

- docs/designs/D-255-fleet-measurement-the-system-the-workers-and-the-assignments.md names this story as the first telemetry dependency; align usage field names with the OpenTelemetry GenAI semantic conventions (gen_ai.usage.input_tokens / output_tokens) so fleet telemetry is readable by standard backends.
