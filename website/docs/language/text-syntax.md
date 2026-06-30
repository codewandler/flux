---
title: Text syntax
---

# Flux-Lang text syntax

The text form is the public, human-readable way to write Flux-Lang. It compiles to the same `DraftAst`
as the JSON wire format.

## Flow

```flux
flow check-readme(path: String) -> String
  $content = read($path)
  return $content
```

The `flow` header may include a name, typed parameters, and a return type.

## Bind, call, and return

```flux
flow summarize
  $src = read("README.md")
  $summary = ai.reason("Summarize this file", ctx: $src)
  return $summary
```

- `$name = ...` binds a value to a symbol.
- `do op(...)` or `op(...)` calls an operation.
- `return ...` ends the flow with a value.

## Branching

```flux
flow route-ticket(kind: String)
  match $kind
    case "bug"
      return "triage-bug"
    case "billing"
      return "triage-billing"
    default
      return "triage-general"
```

`match` is deterministic. `route` is the bounded model-routed form: a selector may choose among
declared labels, but cannot invent new branches.

## Context packs

```flux
flow explain-failure
  $src = read("crates/flux-lang/src/runtime.rs")
  $tests = cargo_test({args: ["-p", "flux-lang"]})
  ctx $debug
    purpose "explain a failing flux-lang test"
    budget 9000
    include $src, $tests
  $answer = ai.reason("Find the likely cause", ctx: $debug)
  return $answer
```

`ctx` builds a bounded context pack from existing symbols. The runtime enforces the character budget at
node evaluation before a consuming model op sees the pack.

## Guard rails

```flux
flow resilient-fetch(url: String)
  timeout 30000 -> $page
    $page = web_fetch($url)
  return $page
```

Flux-Lang has explicit nodes for reliability constraints, including `assert`, `try`, `retry`,
`fallback`, `timeout`, `budget`, `confirm`, `scope`, `saga`, `once`, and `checkpoint`.

## Composite ops

A `.flux` module can define reusable operations from ordinary Flux-Lang:

```flux
op repo-health(path: String, prior: Ctx) -> Health
  description "Check git state and summarize failures"
  risk "medium"
  idempotency "idempotent"
  effects [read, process, local_system]

  $status = git_status()
  $tests = cargo_test({args: ["--workspace"]})
  ctx $pack
    purpose "repo-health"
    budget 8000
    include $prior, $status, $tests
  return {status: $status, tests: $tests}
```

Composite ops are scoped sub-flows. Their inner calls still cross the normal runtime safety envelope.

## JSON escape

The formatter/parser can round-trip every AST node. Nodes without native text spelling use:

```flux
@json {"kind":"peek","name":"survey"}
```

Treat `@json` as an escape hatch, not the preferred authoring style.
