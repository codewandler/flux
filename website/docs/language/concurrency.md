---
title: Concurrency
description: Concurrent fan-out with parallel and first-success racing with race — deterministic results from concurrent execution.
---

# Concurrency

Flux-Lang has two concurrency nodes. `parallel` fans out work and waits for every branch. `race`
takes the first successful branch and cancels the rest. Both keep the run trace deterministic, and
every operation in every branch still crosses the safety envelope.

## `parallel` — concurrent fan-out

Each branch is introduced by a `branch $name` arm; the branch body's last expression becomes
the value bound to `$name` after the join:

```flux
parallel
  branch $readme
    $readme = read("README.md")
  branch $todos
    $todos = grep({pattern: "TODO", glob: "*.rs"})

$report = fmt("readme: {readme}\ntodos: {todos}")
```

After the block, `$readme` and `$todos` are ordinary bound symbols.

**Deterministic output.** Branches execute concurrently, but each writes to a buffering sink;
after the join, results and events are merged in **declaration order**. Output never
interleaves, so the run trace of a parallel block reads the same way every time.

**Failure.** When a branch fails, the completed branches' buffered output and steps are still
merged — a deterministic prefix — before the error propagates.

**Scoping and constraints** (enforced by the analyzer):

- Branch names must be unique.
- `return` inside a branch is rejected — bind in the branch, return after the join.
- Two branches binding the **same symbol** (including inner binds) are rejected: cross-branch
  binds would race.
- A symbol bound inside a branch body (other than the branch's own result) is not visible
  outside that branch.

A `parallel` with one branch is valid and degenerates to a sequential bind.

## `race` — first success wins

`race` starts all branches together and completes as soon as one branch **succeeds**. It has
no native text spelling yet — write it with the `@json` escape (or directly in the JSON wire
form):

```flux
@json {"kind": "race", "timeout_ms": 5000, "bind": "result", "branches": [{"name": "fast", "body": [{"kind": "call", "op": "bash", "args": [{"kind": "lit", "value": "fast-path.sh"}]}]}, {"name": "slow", "body": [{"kind": "call", "op": "bash", "args": [{"kind": "lit", "value": "slow-path.sh"}]}]}]}
```

Semantics:

- **First *success* wins** and its result binds to `bind`. A failing branch does not win, but
  it does not abort the race either — the others keep running.
- **`timeout_ms` is required.** If the deadline expires before any branch succeeds, the node
  errors with a timeout.
- **All branches failed** is a joined branch error — reported distinctly from a timeout, so
  you can tell "everything broke" from "nothing was fast enough".
- **Losing branches stay on the books.** Their already-dispatched steps remain in the step
  count and the trace — audit parity with the event log, and an enclosing `budget` counts
  them.
- Branch names must be unique.

## Choosing between them

| You want | Use |
|---|---|
| All results, combined afterwards | `parallel` |
| The fastest of several equivalent paths | `race` |
| Alternatives tried *in order*, not concurrently | `fallback` — see [Control flow](./control-flow.md) |

Concurrency does not weaken the envelope: a branch cannot dispatch anything the same plan
could not dispatch sequentially, and approvals still gate risky steps.

## Related docs

- [Control flow](./control-flow.md) — sequential alternatives and list iteration.
- [Reliability & guard rails](./reliability.md) — timeouts, retries, and failure handling.
- [Execution model](./execution-model.md) — cancellation and error behavior.
