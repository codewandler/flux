---
title: A ten-minute tour
description: An example-driven walk through Flux-Lang — flows, calls, pure data shaping, branching, concurrency, guard rails, and context packs.
---

# A ten-minute tour

This tour builds one small Flux-Lang vocabulary at a time: flows, calls, pure values, branches,
iteration, concurrency, guard rails, and context packs. Every snippet uses current syntax and can be
pasted into a `.flux` file.

## A minimal flow

A `.flux` file contains one or more flows. The `flow` header is always required; the body is a
sequence of statements, indented two spaces.

```flux
flow hello -> String
  $when = now()
  $greeting = fmt("hello — the time is {when}")
  return $greeting
```

```bash
flux flow run hello.flux
```

`$when = now()` **binds** the result of the `now` operation to a symbol. Symbols are immutable
named values — `{when}` inside a string interpolates the bound value at evaluation time.
`return` ends the flow with a value. This flow never touches a model, so it runs without any
API credentials.

## Calls and named arguments

Operations take **named arguments as a single object**. An op with one required parameter
accepts a bare value as sugar:

```flux
flow todo-scan
  $readme = read("README.md")
  $hits   = grep({pattern: "TODO", glob: "*.rs", max_results: 50})
  return $hits
```

A bare call without a bind runs an operation for its side effects:

```flux
  git_stage(["."])
  git_commit("chore: update generated docs")
```

## Pure data shaping — no shell required

Extracting a field, formatting a string, or assembling a record are **pure nodes**: no IO, no
approval pause, no shelling out to `bash`.

```flux
flow price-check
  $raw   = web_fetch("https://api.coingecko.com/api/v3/simple/price?ids=bitcoin&vs_currencies=usd")
  $price = $raw.bitcoin.usd
  $label = fmt("BTC is at {price} USD")
  return { price: $price, label: $label }
```

`$raw.bitcoin.usd` is field access into a JSON value. `fmt("…")` interpolates bound symbols.
`{ price: $price, label: $label }` is a **value template** — a record assembled from computed
symbols. More in [Pure data](./pure-data.md).

## Branching

`when`/`else` branches on truthiness; `match` branches exhaustively on a bound value:

```flux
flow check-tree
  $status = git_status()
  when $status
    $verdict = fmt("tree is dirty:\n{status}")
  else
    $verdict = "tree is clean"
  return $verdict
```

```flux
flow triage(ticket: Ticket)
  $sev = $ticket.severity
  match $sev
    case "critical"
      do page_oncall $ticket
    case "low"
      do backlog_add $ticket
    default
      do triage_queue $ticket
```

`match` compares by JSON equality and errors on an unmatched subject with no `default` — the
exhaustiveness guard-rail.

## Iteration

`each` maps a list through a body. `-> $collect` gathers each iteration's last expression into
a list:

```flux
flow read-sources(files: List<String>)
  each $f in $files -> $contents
    read($f)
  return $contents
```

Loops are always bounded: `each` by its list, `repeat` by a required count, `loop` by a
wall-clock deadline. The analyzer rejects unbounded iteration. See
[Control flow](./control-flow.md).

## Concurrency

`parallel` runs independent branches concurrently; each branch's last expression binds to its
branch name after the join:

```flux
flow survey
  parallel
    branch $readme
      $readme = read("README.md")
    branch $todos
      $todos = grep({pattern: "TODO", glob: "*.rs"})
  return { readme: $readme, todos: $todos }
```

Results merge in declaration order, so output is deterministic. See
[Concurrency](./concurrency.md).

## Guard rails

Reliability constraints are nodes, not prompt instructions:

```flux
flow careful-fetch(url: String)
  retry 3 backoff exponential delay 500 -> $page
    web_fetch($url)
  assert $page, "fetch returned nothing"
  return $page
```

```flux
  timeout 30000 -> $out
    $out = bash("slow-build.sh")

  budget 10 -> $summary
    $hits    = grep({pattern: "FIXME", glob: "*.rs"})
    $summary = ai.reason({ask: "Group these FIXMEs: {hits}"})
```

`retry` backs off on transient failures (policy denials are never retried), `timeout` bounds
wall-clock time, `budget` caps how many operations a scope may dispatch. See
[Reliability & guard rails](./reliability.md).

## Context packs

Instead of re-sending raw outputs to the model, a plan selects and budgets exactly what a model
op sees:

```flux
flow explain-failure
  $src   = read("crates/flux-lang/src/runtime.rs")
  $tests = cargo_test({args: ["-p", "flux-lang"]})

  ctx $debug
    purpose "explain a failing flux-lang test"
    budget 9000
    include $src, $tests

  $answer = ai.reason({ask: "What is the most likely cause?", ctx: $debug})
  return $answer
```

The runtime enforces the character budget when the pack is built, and records anything it had
to drop. See [Context packs](./context-packs.md).

## Bounded model routing

`route` is the signature bounded-non-determinism primitive: a selector (typically a
model-backed op) picks **which** declared branch runs — it can never invent a new one:

```flux
flow handle-ticket(ticket: String)
  route classify($ticket)
    case "bug"
      do file_bug $ticket
    case "billing"
      do file_billing $ticket
    default
      do triage $ticket
```

The case set is fixed and validated before the flow runs. The model chooses among them; the
runtime does the rest.

## Returning structured results

A flow's result is a value like any other — assemble it from what you computed:

```flux
flow repo-report
  parallel
    branch $status
      $status = git_status()
    branch $log
      $log = git_log({limit: 10})
  return { status: $status, recent: $log, ok: true }
```

## What the tour skipped

- **Durability** — cross-turn caching (`memo`), suspension (`await`), resume points
  (`checkpoint`), at-most-once effects (`once`), cleanup and rollback (`scope`, `saga`):
  [Durability & cross-turn state](./durability.md).
- **First-success races** and the finer points of `parallel`:
  [Concurrency](./concurrency.md).
- **Multi-flow modules, composite ops, and whole programs** in one `.flux` file:
  [Modules, composite ops & programs](./modules-and-programs.md).

From here, read [Flows & syntax](./flows-and-syntax.md) for the precise rules, or head straight
to the [examples cookbook](./examples.md).

## Related docs

- [Flows & syntax](./flows-and-syntax.md) — the full grammar behind the snippets.
- [Examples](./examples.md) — complete flows you can run directly.
- [Reliability & guard rails](./reliability.md) — constraints and failure handling.
