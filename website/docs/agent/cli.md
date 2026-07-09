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
| `flux plan "…"` | show the plan without running it (`-o json\|yaml` prints and exits) |
| `flux` | interactive REPL |
| `flux tui` | ratatui chat UI with an in-UI approval modal |
| `flux a2a <URL>` | drive a remote [A2A](./a2a.md) agent |
| `flux app run <prog.flux>` | run a [multi-agent program](./programs.md); `--serve <addr>` exposes HTTP/A2A |
| `flux flow run <file>` | execute a stored Flux-Lang flow |
| `flux render <file.flux>` | render a `.flux` file as a syntax-highlighted SVG (`--view source\|tree`, `-o out.svg`) |
| `flux loop show \| eject` | inspect or scaffold the [agent loop](./agent-loop.md) |
| `flux auth status \| login` | manage [provider credentials](./providers.md) |
| `flux sessions` / `flux usage` | list recent sessions / show token + cost accounting |
| `flux plugin …` / `flux preset …` | manage subprocess plugins / prebuilt flows |

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
