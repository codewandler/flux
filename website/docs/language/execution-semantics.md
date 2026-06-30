---
title: Execution semantics
---

# Execution semantics

Flux-Lang execution is deterministic except where a plan explicitly calls a model or waits for
external input.

## Lifecycle

```text
compile or parse -> analyze/lower -> optimize -> execute
```

The analyzer validates operations, bounded control flow, top-level-only suspend points, arity, and
argument type compatibility. The optimizer may parallelize or reuse safe read-only work, but dispatch
authorization remains the runtime floor.

## Symbols and values

Symbols are names. Values are immutable records in the value store. Rebinding a symbol creates another
stored value; it does not mutate the old value.

This is why plans can be audited and replayed: the runtime has a value log and a run trace instead of a
hidden mutable environment.

## Operation dispatch

`call` is the operation boundary. The language knows the operation name and arguments; the host decides
what operation exists and dispatches it through policy, approval, redaction, and guarded IO.

Pure nodes such as `fmt`, `jq`, `expr`, `obj`, `list`, and `ctx` do not perform IO or request approval.

## Suspend and resume

`await` is a top-level suspend point. When reached, the flow returns a suspension record. The engine
persists the flow body and resumes from the next top-level statement when the awaited input arrives.
The already-completed prefix is not re-executed.

`checkpoint` is a durable resume point for re-running the same flow in the same session. It pairs with
`once`, which records successful at-most-once side effects.

## Context budget

`ctx` and `ctx_append` build context packs from existing symbols. If a budget is set, the pack is
shrunk when the node evaluates. Consuming operations receive the bounded pack.
