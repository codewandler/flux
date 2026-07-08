---
title: Durability & cross-turn state
description: Session caching, suspension, durable resume, at-most-once effects, guaranteed cleanup, and compensating transactions in Flux-Lang.
---

# Durability & cross-turn state

Durability nodes let a flow outlive one straight-line execution. They cache work, suspend for external
events, resume long runs without repeating side effects, and unwind partial work when a later step
fails.

None of these nodes has native text syntax yet. In `.flux` files they use the `@json` escape; in the
JSON wire form they are ordinary nodes.

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
@json {"kind": "memo", "name": "survey", "value": {"kind": "call", "op": "read", "args": [{"kind": "lit", "value": "big.log"}]}}
```

The cache key is `(session, symbol name)` — a different session always recomputes. Use it for
expensive deterministic work (large reads, slow searches, model calls) that later turns will
need again.

## `peek` — read a symbol without IO

Returns a symbol's current in-session value, or an empty result if it is not yet bound — a
pure lookup, no filesystem involved:

```flux
@json {"kind": "bind", "name": "prev", "value": {"kind": "peek", "name": "last_result"}}
```

`peek` pairs with `unless` for "skip if already computed" within a plan:

```json
{"kind": "unless",
 "cond": {"kind": "peek", "name": "survey"},
 "body": [
   {"kind": "bind", "name": "survey",
    "value": {"kind": "call", "op": "read", "args": [{"kind": "lit", "value": "big.log"}]}}
 ]}
```

For caching across turns, prefer `memo`; `peek` is the in-plan conditional check.

## `await` — suspend until an event

```flux
@json {"kind": "await", "source": "github.push", "binding": "push"}
```

Reaching an `await` suspends the flow: the runtime records the suspend point, persists the
flow, and returns a suspension. When the awaited input arrives — next turn, next webhook — the
flow resumes at the following statement with the received value bound to `binding` (leniently
coerced through `as_type` for number/bool awaits). The completed prefix is **not**
re-executed.

`await` is **top-level only**: the analyzer rejects it nested inside `when`/`each`/`repeat`
bodies, because v1 keeps resume cursors simple and stable.

## `checkpoint` — durable resume point

```flux
@json {"kind": "checkpoint", "label": "phase-1"}
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
@json {"kind": "once", "label": "send-welcome", "body": [{"kind": "call", "op": "send_email", "args": [{"kind": "var", "name": "welcome_msg"}]}]}
```

This is the guard for "never fire twice" effects — sending an email, charging a card — under
re-execution, retries, or checkpoint fast-forwards. `bind` optionally names the stored result.
The label must be a non-empty literal.

## `scope` — guaranteed cleanup

RAII for flows: optionally run `acquire` (binding its result so the body and cleanup can name
the resource), then run `body`; `finally` **always** runs afterward — on normal completion, an
early `return`, or an error.

```flux
@json {"kind": "scope", "acquire": {"kind": "call", "op": "lock.get", "args": [{"kind": "lit", "value": "deploy"}]}, "bind": "h", "body": [{"kind": "call", "op": "deploy", "args": []}], "finally": [{"kind": "call", "op": "lock.release", "args": [{"kind": "var", "name": "h"}]}]}
```

- The body's result, `return`, or error propagates after `finally` runs.
- A `finally` failure surfaces only when the body itself succeeded — cleanup errors never mask
  the body's own error.
- If `acquire` errors, the resource was never taken, so `finally` does not run.

## `saga` — compensating transaction

For sequences of non-transactional external effects. Each step's body runs in order; after a
step succeeds, its `undo` is registered. If a **later** step fails, the runtime unwinds by
running the registered undos in **reverse** order, then propagates the original error:

```flux
@json {"kind": "saga", "steps": [{"body": [{"kind": "call", "op": "charge", "args": []}], "undo": [{"kind": "call", "op": "refund", "args": []}]}, {"body": [{"kind": "call", "op": "ship", "args": []}]}]}
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
