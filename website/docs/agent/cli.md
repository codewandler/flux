---
title: CLI
description: "The public CLI surface for adaptive turns, authored flow execution, app hosting, plugin management, and diagnostics."
---

# CLI

The `flux` binary is the reference surface for day-to-day use. It runs adaptive agent turns,
executes authored flows, hosts programs, manages providers, and exposes diagnostics.

```bash
flux run "fix the failing test"
flux flow list
flux flow run deploy --arg env=dev --arg replicas=3
flux flow run path/to/flow.flux
flux app run path/to/app.flux
```

During a turn, typed model stages receive only capability-scoped native operation schemas. Read-only
evidence calls may execute through the safety envelope; effectful calls are captured into an action
batch and require a matching approval receipt before execution. See [The agent loop](./agent-loop.md)
and [Safety & approvals](./safety.md).

## Subcommands

| Command | What it does |
|---|---|
| `flux run "…"` | run an adaptive turn (`--yes` auto-approves; `-c` continues the last session) |
| `flux` | interactive REPL |
| `flux tui` | ratatui chat UI with an in-UI approval modal |
| `flux a2a <URL>` | drive a remote [A2A](./a2a.md) agent |
| `flux app run <prog.flux>` | run a [multi-agent program](./programs.md); `--serve <addr>` exposes HTTP/A2A |
| `flux flow list` (`ls`) | list project/global saved flows and composite ops without starting an agent session |
| `flux flow run <name\|file>` | execute a saved flow by name or an existing Flux-Lang file (files win) |
| `flux render <file.flux>` | render a `.flux` file as a syntax-highlighted image (`--view source\|tree`; `-o out.svg` writes SVG, `-o out.png` rasterizes PNG with the embedded font; stdout is SVG) |
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
| `flux changelog [version]` | read the embedded customer changelog (`--all` / `--unreleased`) |
| `flux completion [shell]` | generate a completion script (fish by default) |

## Turn controls

```bash
flux run -m openrouter/google/gemini-2.5-flash --effort low "summarize the docs"
flux run --show-loop "update the changelog"
flux run --max-model-calls 8 "answer with live evidence"
flux run --max-iterations 20 "handle a multi-batch task"
flux run --trace-loop "update the changelog"
flux run --loop loops/support.flux "triage this request"
```

- `--effort low|medium|high|xhigh|max` is retained across the intent, exploration, presentation,
  compaction, cognition, and inherited sub-agent calls the agent owns.
- `--show-loop` reveals typed stages and batch machinery; normal operation calls remain visible.
- `--trace-loop` shows the authored loop's structural Flux nodes.
- `--loop adaptive|FILE` explicitly selects the outer loop. `.flux/agent-loop.flux` is never magic.
- `--turn-budget` bounds cumulative model usage for the turn.
- `--max-model-calls` bounds provider consultations across intent, exploration, repairs, and
  decision resumes for one logical adaptive turn (default: 50).
- `--max-iterations` separately bounds decision/batch iterations in the authored outer loop
  (default: 50; accepted range: 1–1,000).
- `--skill NAME` explicitly enables a discovered skill. Skills do not activate from prompt keywords.

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
only parameters not already supplied deterministically.

## REPL slash commands

| Command | Effect |
|---|---|
| `/model <spec>` | switch model mid-session (for example `/model opus`) |
| `/tools` · `/evidence` | list available operations · show the session's evidence trail |
| `/shell` | explicitly toggle the optional shell group |
| `/sessions` · `/resume <id>` · `/clear` | session management |
| `/compact` | compact older conversation history now |
| `/help` | show the complete current command list |

## Inspect and customize the loop

```bash
flux loop show
flux loop eject
flux run --loop .flux/agent-loop.flux "use my edited loop"
```

`eject` copies the built-in preset but does not activate the file. The analyzer validates an explicit
custom loop before the turn begins.

## Related docs

- [Getting started](../getting-started.md) — the first-run path.
- [The agent loop](./agent-loop.md) — intent, exploration, batches, decisions, and repair.
- [Safety and approvals](./safety.md) — what prompts during CLI execution.
- [Providers and models](./providers.md) — how `-m` resolves.
