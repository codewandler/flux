---
title: A ten-minute tour
description: An example-driven walk through Flux-Lang — flows, calls, pure data shaping, branching, concurrency, guard rails, and context packs.
---

# A ten-minute tour

This tour builds one small Flux-Lang vocabulary at a time: flows, calls, pure values, branches,
iteration, concurrency, guard rails, and context packs. Every snippet parses as-is; complete
flows run as-is in a `.flux` file, and the shorter fragments show a flow body (wrap them in a `flow`
header to run standalone). Every snippet uses the formatter's canonical bare symbols, brace-free
named arguments, named headers, and duration units. See [Symbols](./flows-and-syntax.md#symbols) and
[Named arguments](./flows-and-syntax.md#named-arguments) for details.

## A minimal flow

A `.flux` file contains one or more flows. The `flow` header is always required; the body is a
sequence of statements, indented two spaces.

```flux
flow hello -> String
  clock = now()
  utc = clock.utc
  greeting = fmt("hello — the time is {utc}")
  return greeting
```

```bash
flux flow run hello.flux
```

`clock = now()` **binds** the result of the `now` operation — an object with `unix` and `utc`
fields — to a symbol. `utc = clock.utc` reads one field. Each binding creates an immutable value;
the symbol names the version currently in scope. `{utc}` inside a string interpolates that bound
value at evaluation time. `return` ends the flow. This example never touches a model, so it runs
without API credentials.

## Calls and named arguments

Operations take **named arguments**, written brace-free (`grep(glob: "*.rs", pattern: "x")`). They
lower to the AST's single named-input object. An op with one required parameter accepts a bare value:

```flux
flow todo-scan
  readme = read("README.md")
  hits = grep(glob: "*.rs", max_results: 50, pattern: "TODO")
  return hits
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
  raw = web.fetch("https://api.coingecko.com/api/v3/simple/price?ids=bitcoin&vs_currencies=usd")
  price = raw.bitcoin.usd
  label = fmt("BTC is at {price} USD")
  return { label, price }
```

`raw.bitcoin.usd` is field access into a JSON value (`list[0]` indexes a list). Access is
**strict** — a missing field or out-of-range index is a loud error, not a silent empty — so a
typo fails fast; add a trailing `?` (`raw.bitcoin.usd?`) to read `null` when a field may be
absent. `fmt("…")` interpolates bound symbols. `{ label, price }` is a **value
template** — a record assembled from computed symbols. More in [Pure data](./pure-data.md).

## Branching

`when`/`else` branches on truthiness; `match` branches exhaustively on a bound value:

```flux
flow check-tree
  status = git_status()
  when status
    verdict = fmt("""tree is dirty:
{status}""")
  else
    verdict = "tree is clean"
  return verdict
```

```flux
flow triage(ticket: Ticket)
  sev = ticket.severity
  match sev
    case "critical"
      page_oncall(ticket)
    case "low"
      backlog_add(ticket)
    default
      triage_queue(ticket)
```

`match` compares by JSON equality and errors on an unmatched subject with no `default` — the
exhaustiveness guard-rail.

## Iteration

`each` maps a list through a body. `-> collected` gathers each iteration's last expression into
a list:

```flux
flow read-sources(files: List<String>)
  each f in files -> contents
    read(f)
  return contents
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
    branch readme
      readme = read("README.md")
    branch todos
      todos = grep(glob: "*.rs", pattern: "TODO")
  return { readme, todos }
```

Results merge in declaration order, so output is deterministic. See
[Concurrency](./concurrency.md).

## Guard rails

Reliability constraints are nodes, not prompt instructions:

```flux
flow careful-fetch(url: String)
  retry 3, backoff: exponential, delay: 500ms -> page
    web.fetch(url)
  assert page, "fetch returned nothing"
  return page
```

```flux
timeout 30s -> out
  out = bash("slow-build.sh")
budget 10 -> summary
  hits = grep(glob: "*.rs", pattern: "FIXME")
  summary = ai.reason(ask: "Group these FIXMEs: {hits}")
```

`retry` backs off on transient failures (policy denials are never retried), `timeout` bounds
wall-clock time, `budget` caps how many operations a scope may dispatch. See
[Reliability & guard rails](./reliability.md).

## Context packs

Instead of re-sending raw outputs to the model, a plan selects and budgets exactly what a model
op sees:

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

The runtime enforces the character budget when the pack is built, and records anything it had
to drop. See [Context packs](./context-packs.md).

## Bounded model routing

`route` is the signature bounded-non-determinism primitive: a selector (typically a
model-backed op) picks **which** declared branch runs — it can never invent a new one:

```flux
flow handle-ticket(ticket: String)
  route classify(ticket)
    case "bug"
      file_bug(ticket)
    case "billing"
      file_billing(ticket)
    default
      triage(ticket)
```

The case set is fixed and validated before the flow runs. The model chooses among them; the
runtime does the rest.

## Returning structured results

A flow's result is a value like any other — assemble it from what you computed:

```flux
flow repo-report
  parallel
    branch status
      status = git_status()
    branch log
      log = git_log(limit: 10)
  return { ok: true, recent: log, status }
```

## What the tour skipped

- **Durability** — cross-turn caching (`memo`), suspension (`await`), resume points
  (`checkpoint`), durable effect de-duplication after successful completion has been recorded
  (`once`), cleanup and rollback (`scope`, `saga`):
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
- [Editor setup](./editors.md) — highlighting and LSP support for hand-editing flows.
