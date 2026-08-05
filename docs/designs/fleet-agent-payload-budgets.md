# Fleet agent payload budgets

## Status/Scope

Reviewed for C-560 on 2026-08-05. This design bounds oversized Fleet agent events at their source
and at each immediate parsing, aggregation and provider-request boundary. Nested task sessions
changing the store latest-session target and admitted capability loss are linked follow-ups; they
are not silently included in this scope.

## Observed causal chain

A Fleet child uses `--continue` when a runtime session exists. Adaptive exploration clones the full `state.messages` on every round. Six repair attempts therefore lead to a seventh explore request carrying the accumulated history. In the observed failure, `message_bytes` reached 1,231,783 bytes, while 28 tool schemas were accounted separately. The supervisor previously retained and cloned parsed events into the receipt, state, and persistence paths, multiplying the cost of an already oversized event.

Two existing transport bounds frame the source budget: the managed-child buffer is 256 KiB, and Fleet aggregate capture is 1 MiB.

## Byte domains

Budgets apply to distinct byte domains and must not be conflated. The model request is measured
before provider dispatch; the source NDJSON line is measured after redaction; the durable
`turn.budget`/`model.call` session event contains only numeric accounting; and the Fleet receipt
separately reports its parsed stream totals.

- Adaptive tool result: 64 KiB per structured result.
- Source NDJSON line: 240 KiB including the terminating newline, deliberately below the 256 KiB managed-child cap.
- Parser defense-in-depth: 256 KiB per line.
- Accumulated adaptive message history: 512 KiB.
- Provider request: 1 MiB.
- Fleet aggregate capture: 1 MiB across the managed child stream.

`model.call` diagnostics split request accounting into system, message, and schema bytes, with message bytes further split by message category. Schema bytes remain separate from `message_bytes`.

## Decision

Enforce a 64 KiB limit on each adaptive tool result before it enters reusable message history. Enforce a 240 KiB source NDJSON-line budget, newline included, and independently reject lines above 256 KiB in the parser as defense in depth. Refuse adaptive history above 512 KiB and any provider request above 1 MiB before invoking the provider.

After secret redaction, replace any oversized structured value atomically with omission metadata containing its byte count, configured limit, and SHA-256 digest. Never byte-slice structured data. This preserves valid framing and makes equivalent omitted values correlatable without exposing their payload.

## Omission semantics

An oversized structured value is replaced as a whole after redaction; no prefix, suffix, or partial JSON value is retained. The replacement records only byte count, limit, and SHA-256.

For an oversized `turn_end`, preserve session identity, outcome, usage, and cost. Replace the oversized payload with payload-free omission metadata. The resulting event remains a valid, bounded completion record rather than a truncated line.

If accumulated history or the complete request exceeds its budget, emit a payload-free `turn.budget` event and refuse the call before provider dispatch. Budget diagnostics must not echo the rejected messages, schemas, or structured values.

## Operator diagnostics

- `turn.budget` identifies the exceeded domain, observed byte count, and configured limit without carrying payload data.
- `model.call` reports system bytes, total and per-category message bytes, and schema bytes separately.
- Fleet receipt records `stream_budget`, so operators can distinguish source-line, parser, and aggregate-stream constraints from provider-request limits.
- Omission metadata reports byte count, limit, and SHA-256 only, after redaction.

## Verification

The implementation carries focused coverage at each boundary:

- `adaptive_tool_result_is_bounded_without_retaining_payload`
- `adaptive_history_and_request_budgets_are_hard_and_payload_free`
- `message_layer_metrics_count_payload_categories_independently`
- `in_budget_protocol_line_is_unchanged`
- `oversized_nonterminal_is_replaced_with_event_omitted`
- `oversized_turn_end_preserves_terminal_metadata_and_omits_payload`
- `fleet_agent_capture_drops_an_oversized_event_without_corrupting_terminal_json`
- `fleet_agent_receipt_replaces_oversized_event_with_bounded_summary`
- `fleet_agent_receipt_preserves_an_oversized_terminal_outcome`
- `continued_main_coordinator_gets_three_configured_repositories_as_read_only_roots`

Together these cover atomic post-redaction tool-result omission; a source event larger than the
1 MiB supervisor capture window becoming a valid line below the 240 KiB source budget; 256 KiB
parser defense; bounded `turn_end` preservation; payload-free pre-provider `turn.budget`; 512 KiB
history and 1 MiB request refusal; category-split `model.call` accounting; three configured
read-only repository roots on `--continue`; and Fleet receipt `stream_budget`.

## Follow-ups

- [C-561](../stories/C-561-resume-a-failed-fleet-worker.md) makes the failed-turn recovery path usable.
- [C-562](../stories/C-562-bound-fleet-status-projections.md) keeps retained receipts out of the
  default status projection.
- [C-563](../stories/C-563-document-the-fleet-operator-journey.md) teaches the main-agent control and
  watch loop as one public journey.
- [C-564](../stories/C-564-isolate-nested-fleet-task-sessions.md) prevents nested task sessions from
  changing a worker's continuation target.
- [C-565](../stories/C-565-preserve-admitted-fleet-capabilities.md) preserves the admitted capability
  ceiling across continued turns.

These are explicit post-C-560 contracts and are not silently included in this implementation.
