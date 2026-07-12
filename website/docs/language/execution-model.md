---
title: Execution model
description: How a Flux-Lang plan runs — the compile/analyze/optimize/execute lifecycle, symbols and values, operation dispatch, truthiness, and suspension.
---

# Execution model

This page explains what happens after flux receives a plan: parsing, analysis, optimization,
execution, value storage, dispatch, errors, and suspension. Execution is deterministic except where a
plan explicitly calls a model or waits for external input.

## Lifecycle

```text
parse (or receive JSON) -> analyze -> optimize -> execute
```

1. **Parse.** Text is parsed into the AST; a JSON plan is deserialized into the same AST. The
   two forms are interchangeable from here on.
2. **Analyze.** The analyzer lowers the plan to a typed form and rejects what it cannot reason
   about: unknown operations, wrong arity, incompatible argument types, unbounded loops
   (`repeat` needs a count, `loop` needs a deadline), nested suspend points (`await` and
   `checkpoint` are top-level only), and concurrency hazards (duplicate branch names, `return`
   inside a `parallel` branch, two branches binding the same symbol).
3. **Optimize.** The optimizer may parallelize or reuse provably safe read-only work. It never
   changes what a plan is allowed to do — dispatch authorization remains the runtime floor.
   The scheduler summarizes every top-level statement across its **whole subtree** (nested
   blocks, conditions, templates, call arguments): a statement whose reachable operations are
   all registered and read-only may run concurrently with other such statements when their
   symbol reads and writes are independent. A statement containing a write/network/process
   effect, an **unknown operation** (unknown effects are treated as the most dangerous
   effects), an approval/durability construct (`confirm`, `await`, `checkpoint`, `once`,
   `saga`, `thing`), or a cross-turn rate construct (`throttle`, `debounce`) is a **hard fence**:
   nothing is scheduled across it in either direction,
   so approval order and policy behavior are exactly those of sequential execution. The
   optimized run is observationally equivalent to the sequential one — same bound values,
   same user-visible trace order.
4. **Execute.** The interpreter runs the body top to bottom, dispatching every operation
   through the safety envelope and recording a run trace.

A plan that fails analysis never executes at all. That is the point: malformed plans are
rejected before they can touch the world.

## How a model emits a plan

On the wire, every node is the same shape: a JSON object whose `kind` field names the node type
and whose remaining fields are that kind's properties —

```json
{"kind": "retry", "max": 3, "body": [{"kind": "call", "op": "cargo_test"}]}
```

The planner emits a whole plan as one such tree through a single tool call, and there are two
JSON Schemas that describe it:

- The **strict schema** (`fluxlang schema`) spells out each node kind as its own variant with its
  exact required fields — the full-fidelity contract, right for validators and editor tooling.
- The **model-facing merged schema** (`fluxlang schema --merged`) collapses all node kinds into
  **one** object type: `kind` is an enum of every node kind, and the properties are the union
  across kinds, each optional. It is about a third of the strict schema's size, which matters
  when the schema rides along on every planning call — measured on a fixed planning corpus, it
  matched the strict schema's first-emission acceptance at roughly a quarter less input cost, so
  it is the schema flux's planner advertises by default.

Both describe the same wire format, and the merged form gives up no safety: which fields a kind
requires — and where a node may appear at all (`checkpoint` at top level, pure leaves inside
`obj`/`list`) — is context the analyzer checks anyway, on every plan, whichever schema the
emitter saw. A plan that names a wrong field is rejected with a correction the model responds
to, exactly like any other analysis failure.

## Symbols and values

Symbols are names; values are immutable records in a value store the runtime owns.

- A `bind` (`$x = op(…)`) stores the operation's result and points the symbol at it.
- Rebinding a symbol stores a *new* value — the old value is not mutated and stays addressable
  in the audit trail.
- An unbound symbol reference is a hard error at evaluation time.

Because there is no hidden mutable environment, a run is fully described by its value log and
run trace. That is what makes plans auditable and replayable — you can always answer "what did
this symbol hold when that step ran?"

## Operation dispatch

`call` is the boundary between the language and the world. The language knows only the
operation's name and arguments; the **host** decides what operations exist and routes every
call through one chain:

```text
authorization -> approval -> redaction -> guarded IO
```

No node kind bypasses it — not `parallel` branches, not `retry` bodies, not composite ops. The
catalog of operations a plan can target is the host's concern, not the language's: see
[Operations](./ops.md).

**Pure nodes never dispatch.** `fmt`, `jq`, `expr`, `parse`, the `obj`/`list` value templates,
`peek`, and the context-pack nodes (`ctx`, `ctx_append`) perform no IO and never pause for
approval. Use them instead of shelling out for arithmetic, string formatting, or JSON
extraction — see [Pure data](./pure-data.md).

## String interpolation

Any string literal (and the `fmt` node) may embed `{symbol}` placeholders. Substitution happens
at **evaluation time**, from the symbols bound at that moment. A placeholder whose name is not
bound is left verbatim — no silent data loss. To emit a literal brace, double it: `{{` produces
`{`, `}}` produces `}`.

## Truthiness

All condition positions — `when`, `unless`, `assert`, and the `until` guards of `repeat` and
`loop` — use the same JSON truthiness:

| value | truthy? |
|---|---|
| `null` | no |
| `false` | no |
| `0` | no |
| `""` (empty string) | no |
| `"false"` | no |
| `"0"` | no |
| `[]` (empty array) | no |
| `{}` (empty object) | no |
| anything else | yes |

A tool that returns the string `"false"` reads as falsey, so branching on a shell wrapper's or
boolean tool's textual output works as expected.

Conditions can be a symbol, literal, call, or native expression:

```flux
when $count > 3 && $state == "ready"
  return "go"

repeat 10
  until len($queue) == 0
  do poll
```

Native expression conditions lower to pure `expr` nodes; no tool is dispatched to evaluate them.

## Errors

An errored call aborts the flow: nothing is bound, execution stops, and the error propagates —
unless an enclosing node handles it. `try` catches, `retry` re-attempts transient failures,
`fallback` moves to the next branch. Fatal errors (a policy denial, an unknown op, a type
error) are never retried. See [Reliability & guard rails](./reliability.md).

Inside `parallel`, a failing branch still merges the completed branches' output — a
deterministic prefix — before its error propagates. See [Concurrency](./concurrency.md).

## Suspend and resume

Two nodes make flows outlive a single execution:

- **`await`** suspends the flow at a top-level statement until an external event arrives. The
  runtime records the suspend point and persists the flow; when the awaited input arrives, the
  flow resumes at the next statement with the received value bound. The already-completed
  prefix is **not** re-executed.
- **`checkpoint`** is a durable resume marker: re-running the same flow in the same session
  fast-forwards past the completed prefix — its symbols are still bound and its side effects
  are not repeated.

Both are top-level only, because v1 keeps resume cursors simple and stable. The full
cross-turn story — including `memo`, `once`, `scope`, and `saga` — is in
[Durability & cross-turn state](./durability.md).

## What this buys you

- **Reviewability.** A plan is a value you can read before anything runs.
- **Auditability.** The value log and run trace record what happened, in order, with inputs.
- **Replayability.** Deterministic execution over immutable values means a stored flow can be
  re-run — and a suspended one resumed — without guessing at hidden state.

## Related docs

- [Tooling](./tooling.md) — commands that parse, preview, and execute flows.
- [Types & effects](./types-and-effects.md) — annotations and effect tags the analyzer reads.
- [Durability & cross-turn state](./durability.md) — nodes that suspend and resume execution.
