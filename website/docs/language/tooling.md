---
title: Tooling
description: Running, previewing, and compiling Flux-Lang flows — flux flow run, flux plan, flux run, flux app run, and the fluxlang developer CLI.
---

# Tooling

Flux-Lang files are runnable artifacts. This page covers the commands that execute, preview,
compile, format, and round-trip them.

## `flux flow list` / `run` — discover and execute saved flows

```bash
flux flow list                         # alias: flux flow ls
flux flow run deploy                   # filename stem or declared flow name
flux flow run my-flow.flux
flux flow run deploy --inputs '{"env":"dev"}'
flux flow run deploy --arg env=dev --arg replicas=3
flux flow run deploy --map-inputs "deploy three replicas to dev" -m sonnet
flux flow run my-flow.flux --yes -m anthropic/claude-sonnet-5
```

`list` reads `.flux/flows` (project) and `~/.flux/flows` (global), showing saved flows, composite
ops, declared parameters, and parse errors without starting an agent session. Project definitions
win name collisions. `run` resolves an existing file first; otherwise it looks up the target as a
saved filename stem or declared flow name.

The selected flow is already the plan — no model compiles it. Details:

- Accepts **native text or a JSON AST** — a file starting with `{` is read as the JSON wire
  form; anything else parses as Flux-Lang text. Both load to the same plan.
- The file must contain a bare flow, or a module with exactly one flow (or journey). Composite
  `op` declarations in the module are registered for the run.
- Declared flow parameters are required. Pass a JSON object with `--inputs`, or repeatable
  `--arg key=value` values coerced from the declared type. Args override JSON and the last duplicate
  arg wins. Unknown/missing keys, malformed JSON, and concrete type mismatches fail before effects.
- `--map-inputs <text>` is the explicit model-assisted mode. It maps only still-missing parameters;
  deterministic values win. The mapping is part of the recorded plan and uses the same `-m` as any
  model ops authored in the flow. If deterministic inputs cover the contract, no mapping call occurs.
- The provider is constructed **lazily**: a flow that never reaches a model op (`ai.reason`,
  `task`, …) runs with no API credentials at all.
- The safety envelope applies exactly as in an agent turn: risky steps prompt for approval;
  `--yes` auto-approves.
- An **agent** sees the same catalog and can discover/run saved flows with `flow_list` / `flow_run`
  (whose existing compatibility-lenient input behavior is unchanged) — see
  [Where flows live](#where-flows-live).

## `flux plan` — gather safely, then preview

```bash
flux plan "summarize README.md into SUMMARY.txt"
```

Asks the model to compile the request and shows the settled plan without executing that returned
plan. To ground the proposal, plan mode may first auto-run up to three bounded read-only gather
rounds; they are restricted to low-risk operations and cannot request approval. The final plan
— including every pending write, process, or network effect — is printed and left unexecuted. This
is useful for inspecting what a prompt turns into before allowing any mutation.

## `flux run` — plan and execute

```bash
flux run "add a test for the parser"
flux run program.flux
```

The agent path: the model compiles a plan, the runtime executes it, results feed back, repeat
until done — see [The agent loop](../agent/agent-loop.md). Given a `.flux` **program** file
(one with `agent`/`channel`/`journey` declarations), `flux run` auto-detects it and behaves
like `flux app run` — see [Multi-agent programs](../agent/programs.md).

## The `fluxlang` developer CLI

A standalone binary for working with the language itself, built from the repository
(`cargo run -p codewandler-flux-lang --features cli --bin fluxlang -- <command>`):

| command | does |
|---|---|
| `fluxlang compile [file]` | Flux-Lang text → pretty-printed JSON AST (stdin when no file) |
| `fluxlang render [file]` | JSON AST → human-readable tree (colored on a TTY); not to be confused with `flux render`, which turns a `.flux` FILE into a syntax-highlighted SVG image |
| `fluxlang schema` | Print the JSON Schema of the AST (`--merged` for the compact model-facing form — see [Execution model](./execution-model.md#how-a-model-emits-a-plan)) |
| `fluxlang skill` | Print the language skill (the model-facing language reference) |

`compile` is the inverse of the formatter, which makes it a handy syntax checker:

```bash
echo 'flow t
  $x = read("README.md")
  return $x' | cargo run -p codewandler-flux-lang --features cli --bin fluxlang -- compile
```

## The round-trip guarantee

Text and JSON are two spellings of the same AST, and the toolchain holds a hard invariant:

```text
parse(format(ast)) == ast
```

Native spellings cover every node kind; the rare shapes the text grammar cannot express (such as
non-identifier symbol names) round-trip through the one-line `@json` escape. The invariant is
property-tested in the repository, so a plan can be formatted for review and parsed back without
drift.

## Where flows live

- **Authored text** — your repository; the flux repo's own runnable examples are in
  [`examples/`](https://github.com/codewandler/flux/tree/main/examples).
- **The reusable flows home** — drop `.flux` files (flows, ops, or whole modules) under
  **`.flux/flows`** (project) or **`~/.flux/flows`** (global): `flux flow list` / `run` and the
  agent's `flow_list` / `flow_run` ops share one catalog, while composite `op`s placed there
  **auto-load as callable ops**. (The legacy `.flux/ops` / `~/.flux/ops` dirs are still read.)
- **Planner-emitted JSON** — session storage and `.flux/flows/` in a workspace.

Editing conventions: 2-space indentation, no tabs, one statement per line. See
[Flows & syntax](./flows-and-syntax.md) for the grammar and the
[examples cookbook](./examples.md) for ready-to-run material.

## Editor support

Editor support splits into two pieces: syntax highlighting from the
[`codewandler/flux-tree-sitter`](https://github.com/codewandler/flux-tree-sitter) grammar, and
language intelligence — diagnostics, completion, hover, formatting — from the `flux-lsp` language
server. Per-editor recipes (Helix first, plus Neovim, Zed, and IntelliJ/TextMate) are on the
[Editor setup](./editors.md) page.

## Related docs

- [Editor setup](./editors.md) — highlighting and language intelligence for `.flux` files.
- [Flows & syntax](./flows-and-syntax.md) — write valid `.flux` text.
- [Examples](./examples.md) — complete flows to run.
- [FlowClient](../sdk/flow-client.md) — the SDK lifecycle for parsing and executing flows.
