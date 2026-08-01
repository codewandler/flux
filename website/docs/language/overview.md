---
title: Overview
description: What Flux-Lang is, the mental model behind it, and a map of the language documentation.
---

# Flux-Lang overview

Flux-Lang is the authored workflow language at the center of flux. It places deterministic control
flow around model judgment and runtime authority: typed stages may interpret or propose, while the
runtime analyzes the authored program, approves effects, and executes it. The default conversational
loop does not ask the model to emit per-turn executable Flux. The explicit
[`op.register`](../agent/saved-flows.md#register-an-operation-during-a-turn) exception lets a model
propose source for exactly one composite operation; the host parses, analyzes, scopes, and guards it
before installation rather than executing it on receipt.

Start here if you want the mental model before reading syntax or node reference pages.

A flow is a small, readable program over named values:

```flux
flow release-check
  parallel
    branch status
      status = git_status()
    branch tests
      tests = cargo_test(args: ["--workspace"])
  assert tests, "test run produced no output"
  ctx evidence
    purpose "decide whether this tree is releasable"
    budget 8000
    include status, tests
  verdict = ai.reason(ask: "Is this tree ready to release? Answer with reasons.", ctx: evidence)
  return { status, verdict }
```

Two operations run concurrently, a guard aborts early if the evidence is missing, a budgeted context
pack feeds exactly the selected values to one model call, and the flow returns a structured record.
Every effectful step in this flow—the git call, the test run, the model call—crosses the same
runtime safety envelope before it touches the world.

## What Flux-Lang is

- **An executable AST with a readable text form.** The program is structure, not prose. You can
  inspect it before it runs, diff it, and replay it.
- **A workflow language for agent work.** Calls, binds, branching, iteration, concurrency, guard
  rails, and context management are first-class nodes — not conventions layered on chat.
- **A boundary.** The language makes model calls explicit and separates their typed judgment from
  what the authored flow and runtime enforce (order, bounds, policy, approval, IO).

## What Flux-Lang is not

- **Not a shell script.** There is no ambient environment to mutate; values are immutable records
  reached through versioned symbol bindings, and data shaping happens in pure nodes rather than
  shell-outs.
- **Not the default model output.** The conversational agent uses native tool calling inside typed
  stages; an author, SDK, or app supplies the outer Flux program. The narrow `op.register` exception
  above extends its operation vocabulary without replacing that authored control flow.
- **Not a general-purpose language.** Loops are bounded, recursion is rejected, and the analyzer
  refuses plans it cannot reason about. The language is deliberately small.

## The three front-ends

One flow, three ways to say it. All of them describe the same AST:

- **Text** — `.flux` files. Human-writable, comment-friendly, version-controllable. This is what
  you write in an editor and what the docs mostly show.
- **JSON AST** — the canonical programmatic and storage format used by SDKs, tooling, replay, and
  host-derived execution records. Not meant to be hand-written, and **not** an alternate content type
  for a `.flux` file: the CLI parses `.flux` as text and refuses a JSON body rather than sniffing it.
- **Rust DSL** — the `flux_lang::dsl` builders, re-exported as `flux_sdk::dsl`. Typed, composable
  Rust that constructs a `DraftAst` directly, for embedders who want the compiler to check the
  program they are assembling. See the [SDK overview](../sdk/overview.md).

The forms are semantically identical: a `.flux` file parses to exactly the AST the JSON expresses,
the DSL builds that same tree in Rust, and the formatter turns any AST back into canonical text. The
same flow, two of the three ways:

```flux
flow check-readme
  src = read("README.md")
  return src
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

Humans usually write the text form; SDK builders and host tooling may construct the AST directly,
either as JSON or through the Rust DSL. All three are normalized into the same AST before analysis
and execution. Every node kind has a native text spelling; a one-line
`@json` escape remains for the rare shapes the text grammar cannot express. The
[node reference](./node-reference.md) covers every kind in both shapes.

## How a flow runs

A flow moves through a fixed lifecycle: the text is **parsed** (or the JSON deserialized) into the
AST, the **analyzer** lowers it to a typed form and rejects malformed flows (unbounded loops, unknown
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
`flux flow run`, `flux run`, `flux app run`, and the standalone `fluxlang` workbench.

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
- [Ops](./ops.md) — registered operation contracts used by authored flows and model stages.
- [Tooling](./tooling.md) — running, previewing, and formatting flows.

**Examples**:

- [Examples](./examples.md) — a cookbook of small, complete flows.

## Related docs

- [A ten-minute tour](./tour.md) — learn the language by building one flow.
- [Flows & syntax](./flows-and-syntax.md) — exact text syntax rules.
- [Execution model](./execution-model.md) — what happens after a flow is parsed.
- [Editor setup](./editors.md) — highlighting and LSP support for hand-editing flows.
