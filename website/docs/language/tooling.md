---
title: Tooling
description: Running and inspecting authored Flux-Lang flows with flux, fluxlang, and the editor toolchain.
---

# Tooling

Flux-Lang files are authored runnable artifacts. This page covers the commands that execute, parse,
format, render, and round-trip them.

## `flux flow list` / `run`

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

- A `.flux` file is Flux-Lang **text**. JSON is an AST interchange format, not a second meaning for
  the extension: a `.flux` file whose content is a JSON AST is refused with a parse error, not
  sniffed. SDK, replay, and audit paths still read JSON ASTs directly.
- A file must contain a bare flow, or a module with exactly one runnable flow/journey. Composite
  `op` declarations in that module are registered for the run.
- Declared parameters are required. Use `--inputs` or repeatable typed `--arg key=value`; unknown,
  missing, malformed, or incompatible values fail before effects.
- `--map-inputs <text>` is the only model-assisted input mapping mode. It fills only still-missing
  declared parameters; deterministic inputs win and are recorded.
- Providers are constructed lazily. A flow that never reaches a model-backed operation runs without
  provider credentials.
- Every operation uses the same safety envelope; `--yes` auto-approves calls for trusted headless use.
- Agents discover the same catalog through `flow_list` and execute a selected flow through
  `flow_run` in the current guarded session.

## `flux run` — the adaptive agent

```bash
flux run "add a test for the parser"
flux run --show-loop "explain and fix the failure"
flux run --loop loops/review.flux "review this change"
flux run program.flux
```

The conversational path does not compile the request into Flux. Typed stages detect intent and use
exact provider-native operation schemas; an authored Flux-Lang loop controls evidence gathering,
questions, action-batch approval, execution, and presentation. See
[The agent loop](../agent/agent-loop.md).

Given a `.flux` **program** with `agent`/`channel`/`journey` declarations, `flux run` auto-detects it
and behaves like `flux app run`; see [Multi-agent programs](../agent/programs.md).

## `flux loop`

```bash
flux loop show
flux loop eject
flux run --loop .flux/agent-loop.flux "use the edited loop"
```

`eject` copies the built-in adaptive preset for study or editing. The file is not an implicit
override; select it explicitly through `--loop`, `[agent] loop`, an `AgentSpec`, role, or app agent.

## The `fluxlang` developer workbench

The standalone binary works on the pure language without assembling the agent stack:

```bash
cargo run -p codewandler-flux-lang --features cli --bin fluxlang -- <command>
```

| command | does |
|---|---|
| `fluxlang compile [file]` | parse Flux-Lang text and print its JSON AST (stdin when no file) |
| `fluxlang render [file]` | render a JSON AST as a readable tree; `flux render` instead creates an SVG from a file |
| `fluxlang schema` | print the strict AST JSON Schema; `--merged` prints the compact workbench form |
| `fluxlang skill` | print the authored-language reference skill |

Here `compile` is ordinary source-to-AST tooling, not natural-language generation. It is the inverse
of formatting and makes a useful syntax/round-trip check. The analyzer—not either schema—remains the
semantic authority for required fields, placement, operation types, and bounds.

## Where flows live

- **Authored source** lives in your repository; flux's examples are in
  [`examples/`](https://github.com/codewandler/flux/tree/main/examples).
- **Reusable flows and composite ops** live under `.flux/flows` or `~/.flux/flows`. The CLI catalog
  and `flow_list`/`flow_run` share precedence and parsing.
- **JSON AST and host-derived flow records** may appear in SDK, replay, and audit storage. They are
  representations of authored or host-constructed execution, never model output.

Use two-space indentation and no tabs. One statement per line is the house style rather than a
grammar rule — an argument list, object, or array may span lines inside its delimiters. See
[Flows & syntax](./flows-and-syntax.md) and [Examples](./examples.md).

## Editor support

Syntax highlighting comes from
[`codewandler/flux-tree-sitter`](https://github.com/codewandler/flux-tree-sitter). Diagnostics,
completion, hover, and formatting come from `flux-lsp`. See [Editor setup](./editors.md).

## Related docs

- [Flows & syntax](./flows-and-syntax.md) — write valid `.flux` text.
- [Execution model](./execution-model.md) — analyze and execute authored flows.
- [Examples](./examples.md) — complete flows to run.
- [FlowClient](../sdk/flow-client.md) — the SDK lifecycle for parsing and executing flows.
