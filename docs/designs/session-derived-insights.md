# Session-derived insights

**Status:** implemented (C-490) · **Pillar:** Core

## Contract

`flux insights` reads events whose timestamps fall in the machine's current local calendar day.
`/insights [direction]` reads the active session's complete durable stream. Correlated child streams
contribute delegated turns and operation detail, while usage already rolled up onto the parent is not
counted twice.

The host derives the scope, counts, outcomes, durations, iterations, provider usage/cost, operation
counts/errors, approval denials, subjects, and bounded turn records. The model receives that structured,
redacted packet as data and makes one tool-free request whose only job is narration. `direction` can
emphasize part of the packet; it cannot change the selected events or computed facts.

Operator-facing operation counts exclude the authored agent loop's own machinery operations (intent
detection, exploration, plan finalization, and presentation); model calls and iterations account for
that work separately.

## Bounds and safety

- Aggregate facts cover every selected event. Detail is newest-first inside a 64 KiB UTF-8 packet;
  omission is counted and disclosed, and cuts stay on character boundaries.
- Tool-result bodies never enter the packet. User input, assistant answers, subjects, and model output
  pass through `flux-secret` credential-shape redaction.
- The summary request advertises no operations, disables thinking, and caps output at 1,024 tokens.
- Empty daily activity prints zero facts and spends no provider call. Cancellation or provider failure
  keeps the facts visible and persists only whatever usage the attempted call reported.
- Generated prose is display-only. The call itself is recorded as unscoped `CallUsage` on the active or
  newest selected root session, so accounting remains durable without inventing a conversation turn.

## Surfaces

The standalone command writes facts and summary to stdout and accepts the conventional `-m/--model`
override. REPL and TUI slash commands use the live engine provider/model. The TUI treats insights as an
idle-only cancellable maintenance action and renders the result as a notice, not an assistant message.

## Compatibility note

An unscoped modern call can coexist with legacy turns that only carry `TurnEnded.usage`. Usage folds
therefore select per-call records per covered turn and fall back per uncovered legacy turn; a stream-wide
"any CallUsage" switch would silently drop historical spend and is not acceptable.
