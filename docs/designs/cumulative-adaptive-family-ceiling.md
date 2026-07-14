# Cumulative adaptive capability-family ceiling

**Status:** shipped · **Story:** [A-83](../stories/A-83-bound-cumulative-adaptive-families.md)
· **Pillar:** Agent

## Problem

Adaptive intent accepted at most four capability families, and one `signal_capabilities` call also
accepted at most four names. Signals are monotonic for the turn, however: Flux appended new names to
the durable declaration. An intent selecting four families followed by a signal selecting one new
family therefore created five active families. The oversized union was only rejected if its exact
operation or schema expansion happened to cross a separate provider budget.

That made the public four-family bound a per-call hint rather than a turn invariant and allowed a
consumer that had prevalidated every legal four-family selection to encounter a deterministic late
runtime failure.

## Decision

`MAX_FAMILIES` governs the deduplicated accumulated family union for the whole adaptive turn.
`apply_capability_signal` validates every requested name, appends only names not already active, and
then checks the union before calling `selected_specs` or updating `AdaptiveState`. A rejected signal
leaves both the declaration and selected operation list unchanged.

The signal operation's input schema retains `maxItems: 4` as an early provider-side payload bound,
and its description states the cumulative limit. Re-signalling an existing family is idempotent and
does not consume capacity.

The family-count limit remains distinct from the exact selected catalog limits of 64 unique
operations and 128,000 serialized schema characters. Those catalog limits are evaluated only after
the family union passes the cumulative bound.

## Compatibility and safety

Valid one- through four-family turns are unchanged, including later semantic expansion. This check
does not grant or remove authority: live registry, permission, authored `with_tools`, approval, and
guarded execution checks still apply independently.

Embedding consumers may safely validate every reachable family subset up to size four. They should
also retain a boundary regression proving a fifth cumulative signal is refused before catalog
expansion, so a dependency upgrade cannot silently widen the reachable set.
