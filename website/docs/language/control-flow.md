---
title: Control flow
description: Branching, iteration, and sequencing in Flux-Lang — when, unless, match, route, fallback, repeat, each, loop, and seq.
---

# Control flow

Every branch and loop in Flux-Lang is a node in the plan — visible before execution, bounded by
construction, validated by the analyzer. This page covers the deterministic constructs plus
`route`, the one place a model gets a (bounded) say.

## `when` / `else`

```flux
when $ok
  bash("echo yes")
else
  bash("echo no")
```

The condition may be a symbol, a call, or a literal — calling an op directly as the condition
is valid:

```flux
when path_exists("Cargo.toml")
  $out = cargo_check()
```

The `else` branch is optional. Conditions use JSON truthiness — see the
[execution model](./execution-model.md). With nested `when`, each `else` belongs to the `when`
at its own indentation level.

## `unless`

Sugar for "when not". Use it for guard clauses; it takes no `else`:

```flux
unless $already_built
  bash("cargo build")
```

## `match` — deterministic multi-way branch

`match` compares a **bound value** against literal cases by JSON equality and runs the first
match. To branch on an op result or a field, bind it first:

```flux
$kind = $report.kind
match $kind
  case "pass"
    $msg = "all green"
  case "fail"
    $msg = fmt("failures:\n{report}")
  default
    $msg = "unknown report kind"
```

- The subject must be a symbol or literal — not an inline call.
- Comparison is JSON equality, so a *string* subject never equals a *numeric* case value.
- At least one `case` is required. If nothing matches and there is no `default`, the node
  errors — unmatched input is a bug you hear about, not a silent fall-through.

## `route` — bounded model routing

`route` looks like `match`, but the subject is a **selector** — typically a model-backed op —
and the cases are string labels. The model picks *which* declared branch runs; it can never
invent a new one:

```flux
route classify($utterance)
  case "bug"
    do file_bug $utterance
  case "feature"
    do file_feature $utterance
  default
    do triage $utterance
```

The case set is fixed and analyzer-validated. If the selector produces a label that matches no
case, `default` runs; if there is no `default`, the route errors. This is the language's
signature *bounded non-determinism* primitive: useful judgment, no invented control flow.

## `fallback` — first useful success wins

Branches are tried **in order**; the first that completes without error and yields a non-empty
result wins. `-> $bind` names the winning result:

```flux
fallback -> $value
  branch
    $value = read("cache.json")
  branch
    $value = web_fetch($url)
```

- A branch **error or empty result** falls through to the next branch.
- Side effects in a losing branch have already happened by the time it falls through — when
  cleanup or compensation matters, reach for `scope` or `saga` in
  [Durability & cross-turn state](./durability.md).
- If branches succeed only with empty results, the first empty success is kept as a last
  resort; if every branch errors, the last error propagates.

Lighter than `try` for graceful degradation: cheap path first, expensive path second.

## `repeat` — bounded counter loop

The count is required; the analyzer rejects unbounded loops.

```flux
repeat 5
  bash("poll.sh")
```

An optional `until` guard is written as the **first** line of the body and evaluated **after**
each iteration — a stop-when-true check:

```flux
repeat 10
  until $done
  $done = bash("poll.sh")
```

## `each` — list-driven loop

Prefer `each` over `repeat` when iterating a known list. Each element binds to the loop
variable; the body runs per element:

```flux
each $f in $files
  $text = read($f)
```

`-> $collect` gathers each iteration's last expression into a list (an empty source list binds
`[]`); `-> flat` concatenates per-iteration lists into one:

```flux
each $f in $files -> $contents
  read($f)

each $dir in $dirs -> flat $all_files
  glob({pattern: "*.rs", path: $dir})
```

The `in` expression must evaluate to a list; anything else is a runtime error.

## `loop` — time-bounded iteration

`loop for <ms> every <ms>` runs its body until a wall-clock deadline, sleeping between
iterations. Like `repeat`, an optional `until` guard is the first body line, checked after each
iteration; `-> $bind` captures the last iteration's result:

```flux
loop for 30000 every 2000 -> $last
  until $done
  $done = bash("health-check.sh")
```

If the body errors during an iteration, the loop errors immediately — put a `retry` inside the
body if iterations should survive transient failures.

## `seq` — a named sequential block

`seq` groups statements and optionally binds the block's final result:

```flux
seq -> $result
  bash("echo one")
  $two = bash("echo two")
```

A `seq` without a binding is a plain grouping block.

## Choosing a construct

| You want | Use |
|---|---|
| Branch on a condition | `when` / `unless` |
| Branch on a known set of values | `match` |
| Let the model pick among declared paths | `route` |
| Graceful degradation across alternatives | `fallback` |
| Iterate a list | `each` |
| Retry-style counting loop | `repeat` + `until` |
| Poll until a deadline | `loop for … every …` |
| Name the result of a group of steps | `seq -> $x` |

Concurrent fan-out (`parallel`) and first-success racing (`race`) are covered in
[Concurrency](./concurrency.md); failure handling (`try`, `retry`) in
[Reliability & guard rails](./reliability.md).
