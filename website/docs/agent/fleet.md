---
title: Work boards and the fleet
description: "Hand work to remote flux agents and recover it after a restart, using the board as the run registry."
---

# Work boards and the fleet

A **work board** is a governed list of tasks with real states. The **fleet** operations hand those
tasks to other flux agents running elsewhere, and record who is running what — on the board itself.

The two exist together for one reason: a coordinator that hands work out has to survive being
restarted. If the record of "who is running item 42" lives in the coordinator's memory, a crash loses
the fleet. If it lives on the board, recovery is just reading the board again.

## Why not a datasource?

A [datasource](./datasources.md) is read-only knowledge: you index documents and the agent retrieves
from them. That cannot express work that is *claimed*, *moved*, *retried* and *commented on*. A board
is the write-capable sibling — same governed shape, same concrete permission subjects, but the items
have a lifecycle.

## Declaring a board

A board is declared like a datasource, with a `board:` kind:

```flux
datasource board
  kind "board:markdown"
  path "./board"
```

The declaration's **name becomes the operation prefix**, so the example above generates
`board.list`, `board.get`, `board.create`, `board.transition`, `board.claim`, `board.comment` and
`board.record_dispatch`.

`board:` is its own namespace on purpose. `markdown` already means *a directory of documents to index
as knowledge*, so a board that happens to be stored as markdown files needs a name that cannot be
confused with it. Getting this wrong fails loudly rather than quietly doing the wrong thing:

| You write | You get |
|---|---|
| `kind "board:markdown"` | a durable, file-backed board |
| `kind "board:memory"` | an in-process board, gone when the process exits |
| `kind "markdown"` | a **knowledge** index — no board operations at all |
| `kind "board"` or `kind "board:"` | an error naming the available backends |
| `kind "board:something"` | an error naming the available backends |

### Choosing a backend

| Backend | Storage | Survives a restart? |
|---|---|---|
| `board:markdown` | one markdown file per item under `path`, plus a derived index | **yes** |
| `board:memory` | in the process | no |

`path` is resolved relative to the **program file's** directory, not the launch directory, and the
board inherits the session's guarded filesystem root — it cannot read or write outside it.

Anything that depends on recovery needs `board:markdown`. `board:memory` is for a single run and for
tests; its storage *is* the process it would have to outlive.

The derived index is never authoritative. Listing rescans the item files and rewrites the index from
what it finds, so an index that has drifted — or been hand-edited — cannot cause the board to report
something untrue.

## The item lifecycle

Items move through `ready` → `claimed` → `in_progress` → `review` → `done`, with `blocked` and
`failed` off to the side. `board.transition` is the **only** way into the state machine and it
validates the edge first: an illegal move is an error that writes nothing, so a failed transition
cannot leave an item half-moved.

`board.claim` takes ownership and is idempotent for the current holder — a second claim by the same
assignee succeeds, a claim by anyone else is a conflict. That is what makes it safe for several
coordinators to work one board.

The `failed` → `ready` retry edge increments the item's attempt count. No other edge touches it, so
"how many times has this been tried" stays trustworthy.

## Handing work to a worker

Three operations talk to a remote flux agent over A2A:

| Operation | What it does |
|---|---|
| `fleet.dispatch` | hand a task to a worker without waiting; returns the worker's task handle |
| `fleet.status` | poll a handle |
| `fleet.cancel` | stop a run |

A worker is any flux agent served over A2A — `flux app run worker.flux --serve 127.0.0.1:9101` is
enough to be one.

Dispatching with an `item` is what makes the board a run registry: the worker's address and the task
handle are written back onto that item before the dispatch reports success.

```flux
permissions
  allow [send, "board.claim", "board.transition", "fleet.dispatch"]

datasource board
  kind "board:markdown"
  path "./board"

channel cli

trigger on_line
  on "user_input"
  run hand_out

journey hand_out
  flow
    $claimed = board.claim({ id: "item-0001", assignee: "worker-a" })
    $started = board.transition({ id: "item-0001", to: "in_progress" })
    $run = fleet.dispatch({ worker: "http://127.0.0.1:9101", task: "index the repo", item: "item-0001" })
    send({ channel: "cli", message: fmt("dispatched: {run}") })
    return $run
```

After that call the item on disk carries the two fields that were not there before:

```toml
state = "in_progress"
assignee = "worker-a"
runner = "http://127.0.0.1:9101"
task_id = "t_1"
```

If a dispatch names an `item` but the program has no board to record onto, the operation **refuses
before making any network call** rather than dispatching work that nothing could later find.
Dispatching without an `item` is always allowed — that is a deliberate fire-and-forget.

## Recovering after a restart

This is the property the whole design rests on. Because `runner` and `task_id` are on the board, a
new process holding nothing but the board can find every run that was in flight, and reach the worker
that owns it. There is no second store and no state file to reconcile: restart, re-read the board,
carry on.

Concretely: `board.list` with a `state` filter finds the in-flight items, `board.get` returns an
item's `runner` and `task_id`, and `fleet.status` against those values resumes supervision of a run
the current process never started.

Recording a dispatch **replaces** rather than appends, so a retried item never keeps a stale handle
that would send you after a run that no longer exists. It also writes only those two fields — it is
not a state change, so it cannot move an item behind your back.

## What this costs you in permissions

The fleet operations reach the network at an address the caller supplies, so they are gated
accordingly, and none of it is waived by the operations being available:

- A worker on a **private or loopback address is refused** unless you explicitly allow private
  egress. Reaching `127.0.0.1:9101` for the first time is a decision you make, not a default.
- Every call re-resolves its endpoint through the same egress guard before any request. There is no
  path that skips it.
- Approval is scoped to **that worker's origin**, not to "the fleet". A grant for one worker does not
  silently cover another.
- A worker whose address cannot be determined yields no subject at all, which forces approval rather
  than matching a broad existing grant.

Board writes are gated the same concrete way: each operation reports the single item it touches, as
`<board>/item/<id>`, so a permission scoped to one item cannot widen into another.

## Current limits

Worth knowing before you build on this:

- **One board per program, for dispatch recording.** `fleet.dispatch` takes an item id but no board
  name, so if a program declares several boards the id is ambiguous and no recording is wired.
  Dispatch still works; naming an `item` refuses. One board is the supported shape.
- **No scheduled sweep is shipped.** Recovery is available — a restarted process can re-derive
  everything — but you drive it. There is no built-in journey that periodically reconciles in-flight
  items for you.
- **Board operations return human-readable text**, not structured rows, so a program that wants a
  field out of an item currently has to extract it from that text rather than reading it directly.
- **Workers behind authentication are not reachable yet.** There is no configuration for a worker
  bearer token, so a worker served with authentication required cannot be dispatched to.
- **Two backends exist.** Issue-tracker-backed boards are not shipped.

## See also

- [Datasources](./datasources.md) — the read-only knowledge sibling
- [A2A](./a2a.md) — the protocol workers speak, and how to serve one
- [Safety](./safety.md) — how egress and approval gating work in general
