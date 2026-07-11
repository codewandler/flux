---
title: Durability & cross-turn state
description: Session caching, suspension, durable resume, at-most-once effects, guaranteed cleanup, and compensating transactions in Flux-Lang.
---

# Durability & cross-turn state

Durability nodes let a flow outlive one straight-line execution. They cache work, suspend for external
events, resume long runs without repeating side effects, and unwind partial work when a later step
fails.

All of these nodes have native text spellings; in the JSON wire form they are ordinary nodes.

Two kinds of state are in play:

- **Session state** — the value store (`memo`, `peek`) and rate/coalescing keys. Scoped to the
  session; a new session starts clean.
- **Durable state** — `once` results and `checkpoint` positions, scoped to
  `(session, flow)` and folded from an append-only event log. History is never rewritten.
  With no durable store wired (a throwaway interpreter), `once` runs every time and
  `checkpoint` is a no-op.

## `memo` — compute once per session

Like `bind`, but pinned across turns: if the symbol is already resolved for this session, the
op is skipped and the cached value reused.

```flux
memo $survey = read("big.log")
```

Like `bind`, a `memo` accepts an optional type annotation and an `@effect(tag)` line above it:
`memo $schema: String = read("schema.sql")`.

The cache key is `(session, symbol name)` — a different session always recomputes. Use it for
expensive deterministic work (large reads, slow searches, model calls) that later turns will
need again.

## `peek` — read a symbol without IO

Returns a symbol's current in-session value, or an empty result if it is not yet bound — a
pure lookup, no filesystem involved:

```flux
$prev = peek $last_result
```

`peek` pairs with `unless` for "skip if already computed" within a plan:

```flux
unless peek $survey
  $survey = read("big.log")
```

For caching across turns, prefer `memo`; `peek` is the in-plan conditional check.

## `await` — suspend until an event

```flux
await $push = "github.push"
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
$slot = ai_segment({goal: "Find a free 30-minute slot the caller accepts",
                    tools: ["calendar.read"], max_rounds: 3, until: "slot"})
```

`ai_segment(goal, tools, max_rounds, until?)` hands the model a goal for at most `max_rounds`
planning rounds, confined to `tools` (an out-of-scope op is refused and never runs), exiting early
on a prose answer or when the `until` symbol becomes bound to a non-empty value — then control
returns to the flow. It is a reflexive op like `plan`/`run_plan`: never advertised to the model,
callable only from a pre-authored flow, and everything it dispatches crosses the same approval
envelope.

## `checkpoint` — durable resume point

```flux
checkpoint "phase-1"
```

A top-level marker (like `await`) for long or resumable flows. The first time a run reaches
it, the position is recorded durably. A later re-run of the *same* flow in the *same* session
fast-forwards past the already-completed prefix: its symbols are still durably bound and its
side effects are not repeated — execution continues from the checkpoint. The label names the
phase it closes and must be a non-empty literal.

## `once` — at-most-once side effects

An effect-level `memo`. The explicit label is an idempotency key: the first time the body runs
**to success**, its result is recorded durably; later re-runs in the same session skip the
body and reuse the stored result. A failed body records nothing and is retried.

```flux
once "send-welcome"
  send_email($welcome_msg)
```

This is the guard for "never fire twice" effects — sending an email, charging a card — under
re-execution, retries, or checkpoint fast-forwards. An optional `-> $bind` on the header names
the stored result (`once "charge" -> $receipt`). The label must be a non-empty literal.

## `scope` — guaranteed cleanup

RAII for flows: optionally run `acquire` (binding its result so the body and cleanup can name
the resource), then run `body`; `finally` **always** runs afterward — on normal completion, an
early `return`, or an error.

```flux
scope $h = lock.get("deploy")
  deploy()
finally
  lock.release($h)
```

The `$bind = <acquire>` part of the header is optional — a bare `scope` still guarantees its
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
| Skip recomputation within a plan | `peek` + `unless` |
| Wait for external input mid-flow | `await` |
| Resume a long flow without repeating work | `checkpoint` |
| Guarantee an effect fires at most once | `once` |
| Guarantee cleanup on every exit path | `scope` |
| Roll back completed external effects on failure | `saga` |

These compose: a long flow checkpoints between phases, wraps irreversible effects in `once`,
and guards resources with `scope` — and every op inside still crosses the
[safety envelope](../agent/safety.md).

## Related docs

- [Storage & persistence](../reference/storage.md) — the event stores backing resume and sessions.
- [Time Machine](../agent/time-machine.md) — replay, fork, and diff recorded runs.
- [Execution model](./execution-model.md) — suspension, resume, and deterministic prefixes.
