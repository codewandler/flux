# Adaptive ambient operations

## Problem

Adaptive intent routing intentionally selects the smallest functional capability-family set. A host
may also install channel facilities such as structured presentation or a sanitized progress note.
Treating those facilities as an ordinary family makes a correct functional selection accidentally
remove them. Asking the router to select an unrelated UI family spends a family-budget slot and is
unreliable; making a whole domain ambient would defeat catalog narrowing.

## Decision

`flux.ambient` is a reserved physical tool group for a small, host-authored channel surface. Tools in
that group are excluded from the intent-family index and from `signal_capabilities`, then
deterministically unioned into every adaptive exploration request.

Ambient means visibility, not authority. An ambient operation must still be registered in the live
executor, survive permission and active `with_tools` checks, remain inside any authored model-stage
ceiling, fit in the same 64-operation/128k-schema selected-request budget, and dispatch through the
ordinary executor. Its effects, risk, idempotency, intents, approval, evidence, and cancellation are
unchanged. Because the marker lives on `ToolSpec`, an allowlisted role-derived child receives the
same behavior without mutable global state or a new spawner side channel.

Hosts should use this group only for channel facilities that are useful across functional families;
it is not an escape hatch for domain operations. Essential progress should remain host-derived so a
model-authored status operation is optional and its call cost is explicit.

## Compatibility

No existing group becomes ambient. Existing agents preserve their routing behavior until a host
deliberately assigns `flux.ambient`. The selected operation/schema budget becomes stricter in the
intended way: ambient schemas count in every family combination because they are sent in every
exploration request.

