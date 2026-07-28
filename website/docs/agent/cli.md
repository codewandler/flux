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
| `flux record <name> "…"` | record one live turn as a committed-safe scenario fixture — see the [Agent Lab](../sdk/agent-lab.md) |
| `flux test [name]` | replay recorded fixtures offline as a test gate ($0, no key, no network; exit 1 on a regression) |
| `flux eval <adapter>` | run `mock`, `synthetic`, `terminal-bench`, or combined [evaluations](./improvement.md) |
| `flux auth status \| login` | manage [provider credentials](./providers.md) |
| `flux sessions` / `flux usage` | list recent sessions / show token + cost accounting |
| `flux plugin …` | install, inspect, call, pin, and remove [plugins](../plugins/using-plugins.md) |
| `flux endpoint …` | inspect/import model-safe [endpoint references](./endpoints.md) |
| `flux skill …` | render or install generated Flux skills; see [Skills & roles](./skills-and-roles.md) |
| `flux preset …` | list, inspect, render, or run prebuilt flow recipes |
| `flux changelog [version]` | read the embedded customer changelog (`--all` / `--unreleased`) |
| `flux completion [shell]` | generate a completion script (fish by default) |

### `--store <DIR>` — point the session tools at another store

Sessions normally live in `~/.flux` (`events.db` + `flow.db`). `--store` is a global flag that
redirects that for one invocation:

```bash
flux replay --store tests/scenarios/refund-flow last
flux diff --store tests/scenarios/refund-flow s_1 s_2
flux sessions --store tests/scenarios/refund-flow
```

A scenario fixture written by `flux record` **is** an ordinary store in that layout, so the existing
Time Machine tools open one directly — there is no fixture-specific inspection path to learn.

## Crash recovery and resurrection

With durable session storage, entering a conversation that was killed mid-turn first finishes the
interrupted predecessor from its recorded plan, then runs the new input — every turn-entry point
does this, the same step: a one-shot `flux run` turn, the interactive REPL (at startup and on
`/resume`), and the TUI. Completed statements are fast-forwarded, recorded op results are served
from the cassette, and only the remaining live tail runs through the normal approval envelope. Set
`FLUX_AUTO_RESURRECT=0` to opt out.

`flux sessions` is intentionally read-only: it marks interrupted sessions in the listing, but never
resurrects a turn as a side effect of listing sessions.

### Finding a past session

Plain `flux sessions` scrolls newest-first, which doesn't scale once there are dozens of them.
`--query`, `--file`, `--since`, and `--until` narrow the listing to sessions matching every given
filter — still newest-first, no session id needed up front:

```bash
flux sessions --query "refund"                # sessions whose conversation mentions "refund"
flux sessions --file src/billing/refund.rs    # sessions that touched this file
flux sessions --since 2026-07-01 --until 2026-07-15
```

Matching is a read over the same durable, redacted event log every other session tool uses — no
new index, and a secret's plaintext can never be used as a `--query` to confirm a redacted
session's existence.

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

## Command files

Beyond the built-ins above, `/name args…` also dispatches a **command file**: a Markdown file
discovered from project `.flux/commands` or `.claude/commands`, or user-global `~/.flux/commands` or
`~/.claude/commands` (first-wins in that order; a file named after a built-in is dropped at load —
built-ins always win). `/help` and the TUI slash menu list discovered command files with their
`description` and `argument-hint` frontmatter.

```markdown title=".flux/commands/review.md"
---
description: Review a PR for style and correctness
argument-hint: <pr-number>
---
Review PR #$1 for style and correctness issues.
```

`/review 42` substitutes `$1` → `42` (and `$ARGUMENTS` → the full trailing text, `$2`..`$9` for
further positionals; a missing positional substitutes empty) into the body, then runs the result as
the turn's prompt — exactly as if you had typed it. See
[Claude Code compatibility](./claude-compat.md#slash-commands) for the full precedence rules and
what is deliberately not interpreted (`!`-inline-bash, `@file` refs).

A command file is human-only by default. Adding `agent-triggerable: true` to its frontmatter lets
the *agent* invoke it too, mid-turn, via the guarded `command.invoke` op — subject to policy and
session-discovery gates on top of the flag. See
[Agent-side invocation](./claude-compat.md#agent-side-invocation).

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
