# Fixed compaction history budget

Status: accepted (C-462, 2026-08-03)

## Context

Flux compacts a live session once its serialized conversation history exceeds
`DEFAULT_COMPACT_THRESHOLD_CHARS` (48,000). The value does not consult the selected model's context
window. This looked mis-scaled when expressed as an approximate 12,000 tokens: it is a different
fraction of a small local model, a large hosted model, and a frontier model.

Conversation history is not the whole request. Harness and stage instructions, project context,
enabled skills, and operation schemas consume prompt space independently, and only the history grows
turn by turn. A nominal model maximum therefore does not reveal how much headroom is available for
history or how expensive repeatedly sending that history will be.

Flux has no authoritative context-window field in its provider or runtime contracts. Deriving a value
would require a model-id table that becomes stale as aliases, routed models, local models, and custom
endpoints change. Incorrectly high metadata is unsafe for a small-window model; a conservative
fallback recreates the fixed threshold for every model the table cannot classify.

## Evidence

A read-only 2026-08-03 sweep of the local event store covered 112,724 events / 1,474 streams:

| Measure | Value |
|---|---:|
| `call_usage` rows | 5,095 |
| streams with `call_usage` | 816 |
| complete prompt tokens, p50 | 14,353 |
| complete prompt tokens, p95 | 96,663 |
| complete prompt tokens, maximum | 672,150 |
| message-bearing streams | 1,133 |
| streams with at least four messages | 168 |
| streams with at least twenty messages | 7 |
| mean serialized history for multi-turn streams | 5,474 characters |

“Complete prompt tokens” sums ordinary input, cache-creation input, and cache-read input from each
recorded call. It is not a measure of model capacity; the wide range is precisely why model capacity
alone cannot derive a history budget. The store remains dominated by short sessions and cannot prove
that another numeric default is better for sustained interactive work.

## Decision

Keep 48,000 characters as a **fixed history budget** across models. It caps the request component that
grows with the conversation, gives unknown/local/custom providers a defined safe default, and bounds
the repeated cost and latency of retained history independently of a provider's advertised maximum.

Operators with known capacity and workload requirements continue to tune the existing seam:

1. per-agent `compact_threshold_chars`;
2. `FLUX_COMPACT_CHARS`;
3. `DEFAULT_COMPACT_THRESHOLD_CHARS`.

Zero continues to disable compaction. No runtime behavior or precedence changes in C-462.

## Rejected alternative

A percentage of a model-id lookup table was rejected. It accounts for neither the fixed request
components nor provider framing, and it has no correct answer for an unknown model. A future
headroom-aware design would need authoritative capacity plus sizing of the complete provider-framed
request immediately before dispatch. That feature may still choose an absolute cost/latency ceiling
below the model maximum; it is not a replacement for the default history budget chosen here.
