---
title: Context packs
description: Explicit, budgeted context management — the ctx and ctx_append nodes that decide exactly what a model call gets to see.
---

# Context packs

Context packs make model-visible evidence explicit. Instead of sending every tool output back through
the transcript, a plan names the symbols a model call may see, explains why, and sets a budget the
runtime enforces.

## Building a pack

```flux
flow explain-failure
  src = read("crates/flux-lang/src/runtime.rs")
  tests = cargo_test(args: ["-p", "flux-lang"])
  ctx debug
    purpose "explain a failing flux-lang test"
    budget 9000
    include src, tests
  answer = ai.reason(ask: "What is the most likely cause?", ctx: debug)
  return answer
```

The `ctx` block binds a `Ctx` value (see [Types & effects](./types-and-effects.md)) to `debug`. Its
lines:

| line | required | meaning |
|---|---|---|
| `purpose "…"` | no | why the pack exists — seeds the audit trail and any consuming prompt |
| `budget N` | no | character budget the runtime shrinks the pack to (a `0` budget is rejected) |
| `include a, b` | no | symbols selected into the pack |
| `exclude c` | no | symbols removed from the include set |

`ctx` is **pure**: it selects and labels existing values. No IO happens, and nothing is copied
to a model until a consuming op (here `ai.reason`) actually runs.

## Budget semantics

The budget is enforced **when the node evaluates**, not when the plan is written:

- Members are kept in priority order — visibility tier first (a `pinned` member outranks a
  `visible` one and is never dropped to make room for a plainer member), then declared order.
- Packing is **drop-and-continue**: a member that does not fit the remaining budget is dropped
  and packing continues with the next, so one oversized early member never evicts the smaller
  members after it.
- Every dropped member is recorded in the run trace (a `ctx_shrunk` event) — shrinkage is
  visible, never silent.
- An unbound member contributes nothing rather than erroring.
- The v1 budget counter is a character-based heuristic; members are sized by their stored
  value's JSON length.

The consuming model op receives the bounded pack — never more than the plan declared.

## Appending — `+=`

Packs are extended with the append marker:

```flux
more = read("crates/flux-lang/src/analyze.rs")
debug += more
```

`+=` creates a **new** immutable `Ctx` value with the added members, updates `debug` to resolve to
that version, then re-applies the budget. The prior value remains in the audit trail, so each version
records what the model was allowed to see. Multiple symbols append in one line: `debug += more,
extra`.

## Why this matters

- **Cost control.** Model calls are the expensive step; a budget states the ceiling in the
  plan itself.
- **Signal control.** A model asked to diagnose a failure reasons better over two relevant
  values than over an entire transcript.
- **Auditability.** "What did the model see?" has a first-class answer: the pack, its purpose,
  its members, and anything the budget dropped.

## Guidance

- Name a `purpose` — it documents intent for reviewers and seeds the consuming prompt.
- Set a `budget` on any pack feeding a model op; size it to what the question needs.
- Include the few symbols that matter rather than everything available; use `exclude` to trim
  a broad include set.
- Rebind with `+=` as evidence accumulates instead of building a second pack — the audit
  chain stays linear.

## Related docs

- [Pure data shaping](./pure-data.md) — the other pure-node family.
- [Operations](./ops.md) — model-facing cognition ops that consume packs.
- [Execution model](./execution-model.md) — how symbols and values are stored.
