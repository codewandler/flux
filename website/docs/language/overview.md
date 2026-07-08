---
title: Overview
description: What Flux-Lang is, the mental model behind it, and a map of the language documentation.
---

# Flux-Lang overview

Flux-Lang is the plan language at the center of flux. It is the boundary between model judgment and
runtime authority: the model may propose a typed plan, but the runtime analyzes, approves, and
executes it.

Start here if you want the mental model before reading syntax or node reference pages.

A plan is a small, readable program over named values:

```flux
flow release-check
  parallel
    branch $status
      $status = git_status()
    branch $tests
      $tests = cargo_test({args: ["--workspace"]})
  assert $tests, "test run produced no output"
  ctx $evidence
    purpose "decide whether this tree is releasable"
    budget 8000
    include $status, $tests
  $verdict = ai.reason({ask: "Is this tree ready to release? Answer with reasons.", ctx: $evidence})
  return { verdict: $verdict, status: $status }
```

Two operations run concurrently, a guard aborts early if the evidence is missing, a budgeted context
pack feeds exactly the selected values to one model call, and the flow returns a structured record.
Every effectful step in this plan — the git call, the test run, the model call — crosses the same
runtime safety envelope before it touches the world.

## What Flux-Lang is

- **An executable AST with a readable text form.** The program is structure, not prose. You can
  inspect it before it runs, diff it, and replay it.
- **A workflow language for agent work.** Calls, binds, branching, iteration, concurrency, guard
  rails, and context management are first-class nodes — not conventions layered on chat.
- **A boundary.** The language separates what a model may decide (which declared branch, what to
  put in a plan) from what the runtime enforces (policy, approval, IO).

## What Flux-Lang is not

- **Not a shell script.** There is no ambient environment to mutate; values live in immutable
  symbols, and data shaping happens in pure nodes rather than shell-outs.
- **Not a ReAct transcript.** The model does not improvise the next tool call after each result; it
  emits a whole plan up front, and revision is a new plan.
- **Not a general-purpose language.** Loops are bounded, recursion is rejected, and the analyzer
  refuses plans it cannot reason about. The language is deliberately small.

## The two forms

Every flow has two interchangeable representations:

- **Text** — `.flux` files. Human-writable, comment-friendly, version-controllable. This is what
  you write in an editor and what the docs mostly show.
- **JSON AST** — the wire and storage format. This is what the planner emits, what sessions store,
  and what SDKs pass around. Not meant to be hand-written.

The two forms are semantically identical: a `.flux` file parses to exactly the AST the JSON expresses,
and the formatter turns any AST back into canonical text. The same flow, both ways:

```flux
flow check-readme
  $src = read("README.md")
  return $src
```

```json
{
  "name": "check-readme",
  "body": [
    {"kind": "bind", "name": "src",
     "value": {"kind": "call", "op": "read", "args": [{"kind": "lit", "value": "README.md"}]}},
    {"kind": "return", "value": {"kind": "var", "name": "src"}}
  ]
}
```

Humans write the text form; the model writes the JSON form. A handful of node kinds have no native
text spelling yet — in text they are written with a one-line `@json` escape. The
[node reference](./node-reference.md) covers every kind in both shapes.

## How a plan runs

A flow moves through a fixed lifecycle: the text is **parsed** (or the JSON deserialized) into the
AST, the **analyzer** lowers it to a typed form and rejects malformed plans (unbounded loops, unknown
ops, races between parallel branches), the **optimizer** simplifies what it can, and the interpreter
**executes** the result. Every operation call dispatches through one non-bypassable safety envelope —
`authorization -> approval -> guarded IO` — while pure nodes (formatting, field access, templates,
context packs) never touch IO and never pause for approval. The full lifecycle, symbol semantics, and
truthiness rules are in [Execution model](./execution-model.md).

## Running a flow

You do not need a model to run a stored flow — the runtime parses and executes it directly:

```bash
flux flow run hello.flux
```

Risky steps prompt for approval exactly as they do in an agent turn. See [Tooling](./tooling.md) for
`flux plan`, `flux run`, `flux app run`, and the standalone `fluxlang` CLI.

## Reading path

**Guide** — the language, feature by feature:

- [A ten-minute tour](./tour.md) — build one small program step by step. Start here.
- [Flows and syntax](./flows-and-syntax.md) — files, flow headers, symbols, literals, calls, binds,
  `return`.
- [Control flow](./control-flow.md) — `when`, `unless`, `match`, `route`, `repeat`, `each`, `loop`,
  `seq`, `fallback`.
- [Pure data](./pure-data.md) — computation without IO: `fmt`, field access, value templates,
  `expr`, `parse`.
- [Context packs](./context-packs.md) — `ctx`, `ctx_append`, and budgets: what a model call gets
  to see.
- [Concurrency](./concurrency.md) — `parallel` fan-out and first-success `race`.
- [Reliability](./reliability.md) — `assert`, `retry`, `timeout`, `budget`, `confirm`, and the
  other guard rails.
- [Durability](./durability.md) — `memo`, `await`, `checkpoint`, `once`, `scope`, `saga`.
- [Modules and programs](./modules-and-programs.md) — multi-flow files, composite ops, and whole
  apps in one `.flux` file.
- [Execution model](./execution-model.md) — lifecycle, symbols and values, dispatch, truthiness,
  suspension.

**Reference** — the precise surface:

- [Node reference](./node-reference.md) — every node kind with its JSON wire shape and fields.
- [Types and effects](./types-and-effects.md) — type annotations, effect tags, and the prelude
  artifact types.
- [Ops](./ops.md) — the registered operations the engine advertises to plans.
- [Tooling](./tooling.md) — running, previewing, and formatting flows.

**Examples**:

- [Examples](./examples.md) — a cookbook of small, complete flows.

## Related docs

- [A ten-minute tour](./tour.md) — learn the language by building one flow.
- [Flows & syntax](./flows-and-syntax.md) — exact text syntax rules.
- [Execution model](./execution-model.md) — what happens after a plan is parsed.
