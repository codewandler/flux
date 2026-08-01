---
title: Durability & cross-turn state
description: Session caching, suspension, durable resume, completed-effect deduplication, guaranteed cleanup, and compensating transactions in Flux-Lang.
---

# Durability & cross-turn state

Durability nodes let a flow outlive one straight-line execution. They cache work, suspend for external
events, resume long runs without repeating side effects, and unwind partial work when a later step
fails.

All of these nodes have native text spellings; in the JSON wire form they are ordinary nodes.

Two kinds of state are in play:

- **Session state** — the value store (`memo`, `peek`) and rate/coalescing keys. Scoped to the
  session; a new session starts clean.
- **Durable state** — `once` and `checkpoint` deliberately use different identities. A `once`
  completion is keyed by session, its explicit label, and the canonical body identity. A checkpoint
  position is keyed by session and flow identity (declared name plus canonical body hash); its label
  is descriptive event metadata. Both are folded from an append-only event log, so history is never
  rewritten. With no durable store wired (a throwaway interpreter), `once` runs every time and
  `checkpoint` is a no-op.

## `memo` — compute once per session

Like `bind`, but pinned across turns. `memo` accepts a call expression and caches its result by the
operation plus canonical argument AST within the session. Reusing that call skips the operation and
binds the cached immutable value to the authored symbol again; changing the operation or arguments
recomputes even if the destination name is unchanged.

```flux
memo survey = read("big.log")
```

Like `bind`, a `memo` accepts an optional type annotation and an `@effect(tag)` line above it:
`memo schema: String = read("schema.sql")`.

A different session always recomputes. Use `memo` for expensive deterministic work (large reads,
slow searches, model calls) that later turns will need again. Use an ordinary bind for `fmt`, field
access, `parse`, and other non-call expressions.

## `peek` — read a symbol without IO

Returns a symbol's current in-session value, or an empty result if it is not yet bound — a
pure lookup, no filesystem involved:

```flux
prev = peek last_result
```

`peek` pairs with `unless` for "skip if already computed" within a plan:

```flux
unless peek survey
  survey = read("big.log")
```

For caching across turns, prefer `memo`; `peek` is the in-flow conditional check.

## `await` — suspend until an event

```flux
await push = "github.push"
```

The binding is optional — `await "webhook"` suspends without naming the received value.

Reaching an `await` suspends the flow: the runtime records the suspend point, persists the
flow, and returns a suspension. When the awaited input arrives — next turn, next webhook — the
flow resumes at the following statement with the received value bound to `binding` (leniently
coerced through `as_type` for number/bool awaits). The completed prefix is **not**
re-executed.

`await` is **top-level only**: the analyzer rejects it nested inside `when`/`each`/`repeat`
bodies, because v1 keeps resume cursors simple and stable.

### Flow-driven sessions — `await` as the conversation

Since 0.15.0 the same suspend/resume machinery can drive a whole conversation: a **flow-driven
session** runs an authored flow to its first top-level `await`, surfaces the flow's own authored
prompt (its last emitted view) as the assistant turn, and resumes deterministically on each user
reply — zero model calls for the scripted skeleton, over text or the
[realtime voice channel](../agent/realtime.md#flow-driven-voice) (where the prompts are spoken and
the caller's reply resumes the flow).

Where the flow *does* want model judgment, it delegates a bounded segment:

```flux
slot = ai_segment(goal: "Find a free 30-minute slot the caller accepts", max_rounds: 3, tools: ["calendar.read"], until: "slot")
```

`ai_segment(goal, tools, max_rounds, until?)` starts a bounded native-schema stage, confined to
`tools` (an out-of-scope operation is refused and never runs). Gather-safe calls supply evidence;
effects use the same action-batch approval path as a normal turn. A prose result or typed decision
returns control to the authored flow. `ai_segment` itself is host machinery, never advertised to a
model, and everything it dispatches crosses the same approval envelope.

## `checkpoint` — durable resume point

```flux
checkpoint "phase-1"
```

A top-level marker (like `await`) for long or resumable flows. The first time a run reaches
it, the position is recorded durably. A later re-run of the *same* flow in the *same* session
fast-forwards past the already-completed prefix: its symbols are still durably bound and its side
effects are not repeated — execution continues from the checkpoint. Editing the flow changes its
body hash, so a changed flow does not inherit a stale resume cursor. The label names the phase in the
event log and must be a non-empty literal; it is not the resume key.

## `once` — deduplicate completed effects

An effect-level `memo`. The first time a labeled body runs **to success**, its result is recorded
durably; later re-runs of that same labeled body in the session skip it and reuse the stored result.
A failed body records nothing and is retried. Body identity is part of the key, so two different
bodies may share a human label without suppressing one another.

```flux
once "send-welcome"
  send_email(welcome_msg)
```

`once` is not a transaction with the external system. If an effect succeeds and the process crashes
before the runtime appends `OnceCompleted`, the next run sees no completion record and executes the
body again. Its crash window is therefore **at least once**, even though every recorded completion is
deduplicated on later runs. For a strict no-duplicate requirement such as charging a card, also use
the external operation's idempotency key or transactional guarantee. An optional `-> result` on the
header names the stored result (`once "charge" -> receipt`). The label must be a non-empty literal.

## `scope` — guaranteed cleanup

RAII for flows: optionally run `acquire` (binding its result so the body and cleanup can name
the resource), then run `body`; `finally` **always** runs afterward — on normal completion, an
early `return`, or an error.

```flux
scope h = lock.get("deploy")
  deploy()
finally
  lock.release(h)
```

The `resource = <acquire>` part of the header is optional — a bare `scope` still guarantees its
`finally` block runs.

- The body's result, `return`, or error propagates after `finally` runs.
- A `finally` failure surfaces only when the body itself succeeded — cleanup errors never mask
  the body's own error.
- If `acquire` errors, the resource was never taken, so `finally` does not run.

## `saga` — compensating transaction

For sequences of non-transactional external effects. Each step's body runs in order; after a
step succeeds, its `undo` is registered. If a **later** step fails, the runtime unwinds by
running the registered undos in **reverse** order, then propagates the original error:

```flux
saga
  step
    charge()
  undo
    refund()
  step
    ship()
```

- The unwind is best-effort: an undo failure is recorded but does not stop the remaining
  undos.
- A `return` inside a step is a *successful* early exit and does **not** compensate — when you
  need cleanup on every exit path, use `scope`.
- The use cases are charge-then-refund, create-then-delete, reserve-then-release: partial work
  is rolled back instead of left dangling.

## Choosing a construct

| You want | Use |
|---|---|
| Cache an expensive value across turns | `memo` |
| Skip recomputation within a flow | `peek` + `unless` |
| Wait for external input mid-flow | `await` |
| Resume a long flow without repeating work | `checkpoint` |
| Deduplicate an effect after its completion is recorded | `once` |
| Guarantee cleanup on every exit path | `scope` |
| Roll back completed external effects on failure | `saga` |

These compose: a long flow checkpoints between phases, uses `once` to deduplicate recorded
completions, and guards resources with `scope` — and every op inside still crosses the
[safety envelope](../agent/safety.md).

## Related docs

- [Storage & persistence](../reference/storage.md) — the event stores backing resume and sessions.
- [Time Machine](../agent/time-machine.md) — replay, fork, and diff recorded runs.
- [Execution model](./execution-model.md) — suspension, resume, and deterministic prefixes.
