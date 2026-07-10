---
title: CLI
description: "The public CLI surface for turns, flow execution, app hosting, plugin management, and diagnostics."
---

# CLI

The `flux` binary is the reference surface for day-to-day use. It runs agent turns, previews plans,
executes stored flows, hosts programs, manages providers, and exposes diagnostics.

Common paths:

```bash
flux run "fix the failing test"
flux flow list
flux flow run deploy --arg env=dev --arg replicas=3
flux flow run path/to/flow.flux
flux app run path/to/app.flux
```

During a turn, the model has no directly callable tools. It emits a plan, and each operation in that
plan is dispatched by the runtime. Approval prompts appear when policy, risk, or permission rules
require human confirmation — see [Safety & approvals](./safety.md).

## Subcommands

| Command | What it does |
|---|---|
| `flux run "…"` | plan + run a turn (`--yes` auto-approves; `-c` continues the last session) |
| `flux plan "…"` | preview a plan; a terminal offers to run it, while `-o json\|yaml` prints and exits |
| `flux` | interactive REPL |
| `flux tui` | ratatui chat UI with an in-UI approval modal |
| `flux a2a <URL>` | drive a remote [A2A](./a2a.md) agent |
| `flux app run <prog.flux>` | run a [multi-agent program](./programs.md); `--serve <addr>` exposes HTTP/A2A |
| `flux flow list` (`ls`) | list project/global saved flows and composite ops without starting an agent session |
| `flux flow run <name\|file>` | execute a saved flow by name or an existing Flux-Lang file (files win) |
| `flux render <file.flux>` | render a `.flux` file as a syntax-highlighted SVG (`--view source\|tree`, `-o out.svg`) |
| `flux review --files …` | run the embedded read-only multi-reviewer protocol; Markdown or JSON output |
| `flux loop show \| eject` | inspect or scaffold the [agent loop](./agent-loop.md) |
| `flux fork …` / `flux replay …` / `flux diff …` | branch, replay, and compare recorded runs with [Time Machine](./time-machine.md) |
| `flux eval <adapter>` | run `mock`, `synthetic`, `terminal-bench`, or combined [evaluations](./improvement.md) |
| `flux auth status \| login` | manage [provider credentials](./providers.md) |
| `flux sessions` / `flux usage` | list recent sessions / show token + cost accounting |
| `flux plugin …` | install, inspect, call, pin, and remove [plugins](../plugins/using-plugins.md) |
| `flux endpoint …` | inspect/import model-safe [endpoint references](./endpoints.md) |
| `flux skill …` | render or install generated Flux skills; see [Skills & roles](./skills-and-roles.md) |
| `flux preset …` | list, inspect, render, or run prebuilt flow recipes |
| `flux corpus export` | export accepted NL→Flux-Lang plan pairs as JSONL for advanced training/eval work |
| `flux changelog [version]` | read the embedded customer changelog (`--all` / `--unreleased`) |
| `flux completion [shell]` | generate a completion script (fish by default) |

## Saved flow inputs

Put reusable `.flux` files in `.flux/flows` (project) or `~/.flux/flows` (global). `flux flow list`
shows the same names, parameter lists, parse errors, and project-before-global precedence the agent's
`flow_list` operation sees. Run by filename stem or the name in the `flow` declaration:

```bash
flux flow run deploy --inputs '{"env":"dev"}'
flux flow run deploy --arg env=dev --arg replicas=3
flux flow run deploy --map-inputs "deploy three replicas to dev" -m sonnet
```

Declared parameters are required. Unknown keys, missing values, malformed JSON, and concrete type
mismatches fail before the flow starts. `--arg` overrides `--inputs`, and a later duplicate `--arg`
wins. Natural-language mapping is never implicit: only `--map-inputs` invokes a model, and it maps
only parameters not already supplied deterministically. A flow that is otherwise deterministic can
run without provider credentials.

## Two modes

**normal** (default) plans then runs the plan, gating risky steps as they come. **plan** mode shows the
plan and waits — review or refine it, then approve to run. Toggle plan mode in the REPL with `/plan`,
or one-shot with `flux plan "…"`.

## REPL slash commands

| Command | Effect |
|---|---|
| `/plan` · `/run` | toggle plan mode · run the plan you just reviewed |
| `/model <spec>` | switch model mid-session (e.g. `/model opus`) |
| `/tools` · `/evidence` | list available operations · show the turn's evidence trail |
| `/sessions` · `/resume <id>` · `/clear` | session management |
| `/help` | the full command list |

## Agent loop visibility

The default agent loop is itself a Flux-Lang flow. Normal runs hide this machinery, but you can inspect
it when debugging:

```bash
flux run --show-loop "summarize the docs"
flux loop show
flux loop eject
```

Use [The agent loop](./agent-loop.md) for the public model of what those commands reveal.

## Related docs

- [Getting started](../getting-started.md) — the first-run path.
- [Safety and approvals](./safety.md) — what prompts during CLI execution.
- [Providers and models](./providers.md) — how `-m` resolves.
