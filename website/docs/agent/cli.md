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
flux run path/to/workflows.flux --entry triage --arg queue=new
flux app run path/to/app.flux
```

During a turn, typed model stages receive only capability-scoped native operation schemas. Read-only
evidence calls may execute through the safety envelope; effectful calls are captured into an action
batch and require a matching approval receipt before execution. See [The agent loop](./agent-loop.md)
and [Safety & approvals](./safety.md).

## Subcommands

| Command | What it does |
|---|---|
| `flux run "…"` | run an adaptive turn (`--posture` selects the [autonomy posture](#autonomy-posture---posture); `-c` continues the last session) |
| `flux run <module.flux> --entry <flow>` | select one top-level flow from a multi-flow module, execute it once with `--inputs` / repeatable `--arg`, and exit |
| `flux` | interactive REPL |
| `flux tui` | the full-screen [chat UI](./tui.md) with an in-UI approval sheet; `--fleet[=ROOT]` explicitly attaches the durable Fleet-main operations surface |
| `flux system serve …` | serve one canonical workspace as an authenticated TLS [remote execution system](../topologies.md#local-runtime-remote-system) |
| `flux a2a <URL>` | drive a remote [A2A](./a2a.md) agent |
| `flux app run <prog.flux>` | run a [multi-agent program](./programs.md); `--serve <addr>` exposes HTTP/A2A |
| `flux board …` | inspect and mutate session, repository, or workspace [boards](../coding/boards.md); JSON is the stable automation API |
| `flux fleet …` | schedule and inspect bounded local agents through the durable [fleet](../coding/fleet.md) workflow |
| `flux flow list` (`ls`) | list project/global saved flows and composite ops without starting an agent session |
| `flux flow run <name\|file>` | execute a saved flow by name or an existing Flux-Lang file (files win) |
| `flux catalog core --format json` | export the deterministic, versioned catalogue of foundational operations, language nodes, capabilities, and their JSON Schemas |
| `flux docs [-m <provider/model>] [--bind <ADDR>]` | serve the release-matched site and [Flux-Lang workbench](../language/playground.md); loopback enables guarded scratch runs + LSP, while public binds remain docs-only |
| `flux render <file.flux>` | render a `.flux` file as a syntax-highlighted image (`--view source\|tree`; `-o out.svg` writes SVG, `-o out.png` rasterizes PNG with the embedded font; stdout is SVG) |
| `flux review --files …` | run the immutable embedded read-only multi-reviewer protocol under the fail-closed unattended sandbox; Markdown or JSON output |
| `flux loop show \| eject` | inspect or scaffold the [agent loop](./agent-loop.md) |
| `flux fork …` / `flux replay …` / `flux diff …` | branch, replay, and compare recorded runs with [Time Machine](./time-machine.md) |
| `flux export <run> -o run.html` | render a recorded run — plan tree, per-op results/diffs, cost, timeline, nested sub-agents — as one self-contained, redacted static HTML file; the read-only, shareable sibling of `replay`/`fork`/`diff` |
| `flux record <name> "…"` | record one live turn as a committed-safe scenario fixture — see the [Agent Lab](../sdk/agent-lab.md) |
| `flux test [name]` | replay recorded fixtures offline as a test gate ($0, no key, no network; exit 1 on a regression) |
| `flux eval <adapter>` | run `mock`, `synthetic`, `terminal-bench`, or combined [evaluations](./improvement.md) — the public, in-repo scoring engine; an optional standalone harness benchmark is maintained separately and is not currently published |
| `flux auth status \| login` | manage [provider credentials](./providers.md) |
| `flux sessions` / `flux usage` | list recent sessions / show token + cost accounting |
| `flux insights [-m <provider/model>]` | derive today's sessions, outcomes, time, usage, operations, errors, approvals and subjects from the durable log, then narrate those redacted facts with one tool-free model call |
| `flux context show` | inspect the ordered, body-free harness/profile/project context manifest (`--json`; add `--body` explicitly to print content, `--profile` / `--tool` to model conditional layers) |
| `flux wakeups list \| cancel` | list or cancel a session's pending agent-scheduled wake-ups (`schedule_wakeup`) |
| `flux plugin …` | install, inspect, call, pin, and remove [plugins](../plugins/using-plugins.md) |
| `flux endpoint …` | inspect/import model-safe [endpoint references](./endpoints.md) |
| `flux exchange local start\|status\|stop` | enter the managed local Exchange lifecycle surface; until the signed release/lifecycle contract ships, each verb makes no change and returns a typed `unsupported` refusal. The final lifecycle runs only on the two Linux GNU targets; every other target keeps the command and refuses before effects (`--json` for one machine-readable result). |
| `flux integration connect\|grant\|list\|doctor` | enter labelled Exchange connection management; until the provider connection/grant contracts ship, each command makes no change and returns a typed `unsupported` refusal. Final owner onboarding is Linux-local; the authenticated runtime HTTP client may still use an independently provisioned Linux Exchange from every Flux target. |
| `flux policy simulate <proposed.toml>` | replay a proposed authorization policy against recorded op history — a diff of what it would have newly blocked and newly allowed, before you adopt it; a pure read, `--sessions N` / `--json` |
| `flux skill …` | render or install generated Flux skills; see [Skills & roles](./skills-and-roles.md) |
| `flux preset …` | list, inspect, render, or run prebuilt flow recipes |
| `flux changelog [version]` | read the embedded customer changelog (`--all` / `--unreleased`) |
| `flux completion [shell]` | generate a completion script (fish by default) |
| `flux doctor` | diagnose the install: credentials, plugin-pack integrity, sandbox backend, `events.db` health, egress config, version skew, `[tools] disable`, and config provenance (`--json` for scripting) |

## Global flags

These seven flags are accepted by **every** subcommand — they sit on `flux` itself, so
`flux --sandbox run "…"` and `flux run --sandbox "…"` are equivalent. Everything else is
per-subcommand (see [Turn controls](#turn-controls) for the flags the agent-path commands carry).

| Flag | What it does |
|---|---|
| `--color auto\|always\|never` | When to colorize output. `auto` (the default) colors only when **both** stdout and stderr are terminals and `NO_COLOR` is unset, so `flux usage > report.txt` never embeds escapes. |
| `--store <DIR>` | Read and write sessions in `DIR` instead of `~/.flux` (see [below](#store-flag)). |
| `--add-dir <DIR>` | Grant **read** access to one more directory outside the workspace. Repeatable; writes stay confined to the current directory. |
| `--allow-all-paths` | Lift filesystem confinement entirely — read *and* write anywhere. Dangerous; prints a warning. |
| `--allow-private-net` | Allow egress to private/internal addresses for this invocation only — nothing is persisted. |
| `--sandbox` | Turn on OS-level process sandboxing (bubblewrap on Linux, Seatbelt on macOS) for spawned shell/plugin processes. |
| `--no-sandbox` | Force OS-level sandboxing off — the kill switch, overriding `--sandbox`, `FLUX_SANDBOX`, and config. Conflicts with `--sandbox`. |

The four safety flags are per-invocation overrides layered over persistent configuration, and the
reasoning behind each lives with the model it belongs to:

- `--add-dir` and `--allow-all-paths` widen the **workspace** boundary — see
  [`[workspace]` in the configuration reference](../reference/config.md#skills-and-workspace-access).
  They do not widen what [project context](./project-context.md) reads.
- `--allow-private-net` opens **egress** — see
  [private-network egress](../reference/config.md#private-network-egress) for the scoped
  `[private_net]` grants you should prefer for anything recurring.
- `--sandbox` / `--no-sandbox` control **process** confinement — see the
  [OS sandbox](../security/os-sandbox.md) page for backends, the strictest-wins precedence, and
  what happens when no backend is available.

None of these replace the approval envelope; they change what an *approved* action is allowed to
touch. See [Safety and approvals](./safety.md).

## Local or remote effects

Agent-path commands accept `--remote <HTTPS_URL>` to keep the runtime, model, session, credentials,
and approval UI local while guarded file/process/network effects land on a remote system. With no
flag, effects remain local. Tokens are read from `FLUX_REMOTE_SYSTEM_TOKEN`, or from the environment
variable named by `--remote-token-env`; `--remote-ca <PEM>` adds a private CA. See
[Topologies](../topologies.md#local-runtime-remote-system) for daemon setup, workspace semantics,
the no-sync rule, and which guarantees are enforced on each side.

### `--store <DIR>` — point the session tools at another store {#store-flag}

Sessions normally live in `~/.flux` (`events.db` + `flow.db`). `--store` redirects that for one
invocation:

```bash
flux replay --store tests/scenarios/refund-flow last
flux diff --store tests/scenarios/refund-flow s_1 s_2
flux export --store tests/scenarios/refund-flow last -o refund-flow.html
flux sessions --store tests/scenarios/refund-flow
```

A scenario fixture written by `flux record` **is** an ordinary store in that layout, so the existing
Time Machine tools open one directly — there is no fixture-specific inspection path to learn.

## Diagnostics

`flux doctor` runs a fixed suite of checks over the install and prints a pass/warn/fail line per
check with a one-line fix-it hint on every non-pass:

```bash
flux doctor
flux doctor --json   # machine-readable, for scripting / CI
```

The suite covers provider credentials (including OAuth token expiry), plugin-pack signature/hash
drift, the OS sandbox backend (bubblewrap / `sandbox-exec`), `events.db` integrity and WAL size,
private-network egress config sanity, `[tools] disable` resolution, and version skew against the
latest release. A `WARN` never affects the exit code; the command exits non-zero iff at least one
check `FAIL`s.

## Crash recovery and resurrection

With durable session storage, entering a conversation that was killed after accepting a plan first
finishes that interrupted turn, then runs the new input. A one-shot `flux run` turn, the interactive
REPL (at startup and on `/resume`), and the TUI all use the same step. Completed statements are
fast-forwarded, op results that reached the durable cassette are served without re-dispatch, and the
remaining tail runs live through the normal approval envelope. Set `FLUX_AUTO_RESURRECT=0` to opt
out.

This is at-least-once recovery, not a blanket exactly-once guarantee. If an effect happened but the
process died before its cassette cell was appended, that op can run again. A crash before any plan
was accepted has no durable plan to resume and is reported instead of being reconstructed. Recovery
also covers only durable session events: REPL/TUI drafts and queued-but-unconsumed follow-ups are
process memory, not session state.

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

## Autonomy posture (`--posture`)

Who answers a guarded effect is a named choice, and it selects the approver, the OS-sandbox floor
and the resource budget **together** — see
[Autonomy is a posture](./safety.md#autonomy-is-a-posture) for what each one relies on and what it
does not protect against.

```bash
flux run "refactor the parser"                            # supervised (default): you answer each effect
flux run --posture bounded-autonomy "fix the flaky test"  # never prompt; confined, egress closed, budgeted
flux run --yes "fix the flaky test"                       # the older spelling of the same posture
flux run --posture exploratory "audit this repo for auth bugs"
flux app run agent.flux --posture refusing                # nothing that reaches approval runs
```

- `--posture supervised` (default on interactive surfaces) — a human at the terminal, per effect.
- `--posture bounded-autonomy` — no prompt; authorization policy, a fail-closed sandbox with the
  network **closed**, and resource budgets constrain instead. `--yes` selects this.
- `--posture exploratory` — no prompt, and interruption is the harm: the same fail-closed
  confinement with egress **open**, wider ceilings and an uncapped evidence trail. For research,
  security hardening and long exploration.
- `--posture refusing` — every effect reaching the approval stage is denied. The default on a
  surface with no operator attached (`flux app run <program>`, `flux record`), which also refuse an
  explicit `--posture supervised` rather than silently downgrading it.
- `--yes` together with a contradictory `--posture` is refused, not resolved. `--yes --posture
  exploratory` is fine: both say "do not ask".
- Authorization, guarded IO and the evidence trail do **not** vary with the posture. Approval is the
  only stage of the three with a human in it.

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
- `--max-tokens` caps the output tokens of a single model-stage call (default: 16384; must be ≥ 1).
  A truncated intent, exploration, repair, or presentation stage fails loudly rather than silently
  stopping.
- `--turn-budget` bounds cumulative model usage for the turn — a hard limit, so the turn stops at the
  next safe boundary instead of consulting the model again. See
  [time and token budgets](#time-and-token-budgets).
- `--max-model-calls` bounds provider consultations across intent, exploration, repairs, and
  decision resumes for one logical adaptive turn (default: 50).
- `--max-iterations` separately bounds decision/batch iterations in the authored outer loop
  (default: 50; accepted range: 1–1,000).
- `--skill NAME` explicitly enables a discovered skill. Skills do not activate from prompt keywords.

### Time and token budgets

Budgets use two words, and they do not mean the same thing:

- A **target** is a declared intent. Crossing it warns once and nothing stops:

  ```text
  ⚠ budget target crossed — run total_tokens 1.6k of 1.0k (hard limit 4.0k) · execution continues
  ```

- A **limit** is a hard ceiling. Crossing it stops at the next safe boundary and names exactly what
  was crossed — scope, dimension, spent, limit:

  ```text
  ⚠ budget limit reached — turn total_tokens 4.6k of 4.0k · stopping at the next safe boundary
  ```

Both sides are declared per scope (run, session, turn, or one loop segment) over wall time, model
calls, and input, output and total tokens. `--turn-budget` is the flag-level entry point — a hard
total-token ceiling for one turn; hosts embedding the SDK can declare the full envelope, including
targets and a wall-clock deadline.

A model call already in flight is never reported as stopped: its usage is measured and charged first,
and only then can the next round be refused. The enforcing ledger is also the only accountant — it
publishes one spent-versus-declared projection that these lines, [the TUI header](./tui.md) and the
durable event trail all render, so the figure you read is the figure that stops the run. A dimension
nobody declared shows nothing rather than a zero ceiling.

## Machine-readable output (`--stream-json`)

:::caution
`--stream-json` / `--stream-json-input` is a **preview surface** — the line shapes below are not
yet a compatibility promise. Every line carries `"v": 1` today; a breaking revision bumps that.
:::

`--stream-json` emits one JSON object per line to stdout instead of human-rendered output —
everything a CI job, an editor extension, or another harness needs to drive and observe one turn
without linking `flux-sdk`. Diagnostics still go to stderr, so stdout is `jq`-parseable with no
filtering:

```bash
flux run --stream-json --yes "fix the failing test" | jq -c 'select(.type == "tool_call")'
```

Every line is a projection of a fact the engine already reports through its internal streaming
sink — the same one the plain terminal renders from — so the two never diverge in what they
consider "the plan" or "the result". Line `type`s:

| `type` | When | Key fields |
|---|---|---|
| `turn_start` | once, before the turn begins | `session`, `model`, `input` |
| `plan` | the agent proposes an action batch | `session`, `data` (batch id, action count, risk, the redacted batch) |
| `approval` | that batch is requested/approved/denied | `session`, `phase`, `data` |
| `tool_call` | an operation is about to run | `session`, `dispatch`, `name`, `input` |
| `tool_result` | it finished | `session`, `dispatch`, `name`, `is_error`, `content`, `view`, `duration_us` |
| `steered` | mid-turn guidance was folded in (see below) | `session`, `messages` |
| `turn_end` | once, at the end | `session`, `outcome` (`ok`/`error`), `error` (only when set), `answer`, `usage`, `cost_usd` |
| `error` | the turn itself failed to run | `session`, `message` |

`dispatch` is a process-unique id for one operation. It appears on the `tool_call` line and
again on that call's `tool_result`, so a client pairs the two by identity. Do not pair on
`name` and arrival order: independent read-only operations in one batch run concurrently and
may finish in any order.

`--stream-json-input` additionally reads the same NDJSON framing on stdin, for a multi-message
conversation in one process — requires `--yes` (there is no interactive-approval framing over the
input stream in this preview). A plain line queues the next turn; a line with `"steer": true`
injects into the turn **currently running** instead of waiting for it to finish:

```bash
flux run --stream-json-input --yes -m sonnet <<'EOF'
{"text": "audit crates/flux-cli/src/args.rs for dead flags"}
{"text": "actually, skip anything hidden — focus on the public ones", "steer": true}
EOF
```

A `steer: true` line that arrives with no turn running has nothing to steer, so it becomes an
ordinary next turn instead of being dropped.

Every emitted line is redacted independently of the ordinary tool-result scrubbing — including a
tool call's own input arguments, which the safety envelope's result redaction never touches. See
[Safety and approvals](./safety.md) for the redaction model this builds on.

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

Bare `flux` opens a line-oriented REPL. Its built-in commands:

| Command | Effect |
|---|---|
| `/help` | show the complete current command list |
| `/model <spec>` | switch model mid-session (for example `/model opus`) |
| `/effort [level]` | show or set reasoning effort (`low`, `medium`, `high`, `xhigh`, `max`, `off`) |
| `/tools` · `/evidence` | list available operations · show the session's evidence trail |
| `/shell` | explicitly toggle the optional shell group |
| `/plugin-refresh <name>` | re-read a loaded plugin's operation catalog; the refreshed set is adopted at the next turn boundary (the current turn keeps its catalog) |
| `/session` · `/sessions` · `/resume <id>` · `/clear` | session management (`/sessions --prune` deletes empty sessions) |
| `/compact` | compact older conversation history now |
| `/insights [direction]` | show deterministic facts for the active session, then focus one grounded summary (for example, `focus on blockers`) |
| `/pd <goal>` | plan-and-dispatch: run subtasks as parallel dependency waves |
| `/goal <condition>` | drive turns toward a goal, stopping once it is satisfied |
| `/loop <n> <task>` | repeat a task up to `n` times |
| `/exit` · `/quit` | leave the REPL (`Ctrl-D` also exits; `Ctrl-C` interrupts a running turn) |

The [TUI](./tui.md#slash-commands) has its own, partly different set — it adds `/new`, `/usage`,
`/queue` and `/theme`, shares `/insights`, and does not carry `/pd`, `/goal`, or `/loop`.

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
- [TUI](./tui.md) — keybindings, mid-turn steering, and in-UI approvals for `flux tui`.
- [The agent loop](./agent-loop.md) — intent, exploration, batches, decisions, and repair.
- [Safety and approvals](./safety.md) — what prompts during CLI execution.
- [Providers and models](./providers.md) — how `-m` resolves.
- [OS sandbox](../security/os-sandbox.md) — the reasoning behind `--sandbox` / `--no-sandbox`.
