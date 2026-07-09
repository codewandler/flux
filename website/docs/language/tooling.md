---
title: Tooling
description: Running, previewing, and compiling Flux-Lang flows — flux flow run, flux plan, flux run, flux app run, and the fluxlang developer CLI.
---

# Tooling

Flux-Lang files are runnable artifacts. This page covers the commands that execute, preview,
compile, format, and round-trip them.

## `flux flow run` — execute a stored flow

```bash
flux flow run my-flow.flux
flux flow run my-flow.flux --yes -m anthropic/claude-sonnet-5
```

Runs a checked-in flow **deterministically** — no model compiles anything; the file *is* the
plan. Details:

- Accepts **native text or a JSON AST** — a file starting with `{` is read as the JSON wire
  form; anything else parses as Flux-Lang text. Both load to the same plan.
- The file must contain a bare flow, or a module with exactly one flow (or journey). Composite
  `op` declarations in the module are registered for the run.
- The provider is constructed **lazily**: a flow that never reaches a model op (`ai.reason`,
  `task`, …) runs with no API credentials at all.
- The safety envelope applies exactly as in an agent turn: risky steps prompt for approval;
  `--yes` auto-approves.

## `flux plan` — preview without executing

```bash
flux plan "summarize README.md into SUMMARY.txt"
```

Asks the model to compile the request into a plan and shows it — nothing runs. Useful for
inspecting what a prompt turns into before letting it touch the workspace.

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
| `fluxlang render [file]` | JSON AST → human-readable tree (colored on a TTY) |
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
- **Planner-emitted JSON** — session storage and `.flux/flows/` in a workspace.

Editing conventions: 2-space indentation, no tabs, one statement per line. See
[Flows & syntax](./flows-and-syntax.md) for the grammar and the
[examples cookbook](./examples.md) for ready-to-run material.

## Editor support

Two complementary pieces, matching how modern editors split the work:

- **`flux-lsp` — language intelligence.** A Language Server in the flux repository
  (`cargo build -p flux-lsp`): live error-recovering diagnostics with real spans, completion
  (ops, node-kind keywords, prelude types, in-scope `$vars`), hover (op signatures with
  effects/risk, node-kind docs), and document formatting via the invertible formatter. Helix
  needs config only — see the repo's
  [`.helix/languages.toml`](https://github.com/codewandler/flux/blob/main/.helix/languages.toml);
  any LSP-capable editor can wire the same binary.
- **[`codewandler/flux-tree-sitter`](https://github.com/codewandler/flux-tree-sitter) — syntax
  highlighting.** A tree-sitter grammar with highlight/injection queries for **Helix, Neovim, and
  Zed** (none of which read TextMate grammars). Note that Helix colours via tree-sitter *only* —
  it does not render LSP semantic tokens — so in Helix the grammar provides all colour and the
  LSP provides everything else. Install instructions per editor are in that repo's README.

[`codewandler/flux-editors`](https://github.com/codewandler/flux-editors) additionally ships the
TextMate grammar and the IntelliJ plugin for their own ecosystems.

## Related docs

- [Flows & syntax](./flows-and-syntax.md) — write valid `.flux` text.
- [Examples](./examples.md) — complete flows to run.
- [FlowClient](../sdk/flow-client.md) — the SDK lifecycle for parsing and executing flows.
