# Using flux

flux is a coding/ops agent with one core idea: **the LLM is not the runtime.** Instead of the model
calling tools live, one step at a time, the model is a *compiler front-end* — it turns your request into
a typed **execution plan** (a small Flux-Lang graph), and a deterministic Rust runtime executes that
plan through a safety envelope. You always see the plan before it runs, and the same plan can be re-run.

This page is the practical guide. For the design, see [`docs/designs/flux-flow.md`](designs/flux-flow.md).

## The mental model

Every turn, the model does exactly one of two things:

- **emits a plan** — a graph of operations (`read`, `grep`, `edit`, `repeat`, `when`, …), or
- **answers in prose** — when no operation is needed.

The built-in file operations are: `read` (one file, a list of files, or a glob pattern — single-file
reads get a line-numbered view, multi-file reads return `==> path <==` sections; refuses binary and
guides you to a range for very large files; `read_many` remains only as a legacy alias), `write`
(create/overwrite, returns a diff), `append` (lower-risk add to a file), `edit` (string replace with
progressively looser whitespace/indentation matching and a unified diff), `patch` (line-anchored
`insert_before/after`/`replace_range`/`delete_range`), `glob`, and `grep` (regex by default; pass
`literal` for a plain substring). A file must be **read before you `edit`/`patch` it** — and if it
changed on disk since you read it, the edit is refused so you re-read first. A generic `bash` op
exists but is **off by default** (the `shell` tool group): opt in with `enable_shell = true` in
`.flux/config.toml` or `FLUX_ENABLE_BASH=1` — prefer the dedicated ops.

It has **no live tools.** It can't call `bash` or `read` directly; even reading a file is a node in a
plan. This is what makes a turn auditable: what you see *is* what runs.

```
flow
├─ $readme = read("README.md")
└─ return $readme
```

The runtime executes the plan node by node through the safety envelope (permissions, approval, secret
redaction), stores each result as a named symbol, and feeds it back so the model can plan the next step.
A later node reuses an earlier result by name — `$readme` to pass the whole value as an argument, or
`{{readme}}` inside a string to embed it (e.g. in a sub-agent prompt); the runtime substitutes the
stored value at execution.

A trivial or already-actionable request costs exactly one (or two) model calls, same as always. A
complex or context-hungry request first gets a bounded, read-only "gather" pass — the model looks
around before committing to a plan, capped at a few rounds — rather than guessing the whole task in
one shot. See [`docs/agent-loop.md`](agent-loop.md) for the phased loop that drives this.

## Two modes: normal and plan

| Mode | What a turn does |
|---|---|
| **normal** (default) | the model plans → the runtime **shows the plan, then runs it** (risky steps prompt for approval) |
| **plan** | the model plans (auto-running a bounded read-only look-around first, if it needs one) → the runtime **shows the final plan but does NOT run it**; you review/refine, then approve to run |

Plan mode is for "let me see (and shape) the whole plan before anything happens." Normal mode just does
the work, gating risky steps as they come.

A complex or context-hungry `flux plan`/`/plan` prompt gets the same bounded, read-only "gather" pass
normal mode does (see above): the model may look around — read a few files, grep, list a directory —
before committing to the real plan. Gather is compile-time enforced non-mutating (no write, no
destructive op), so it runs automatically without a prompt, exactly the trust `run_plan` already
grants a non-mutating plan. Only the **final** plan — the one that would make the actual change — is
ever shown-and-not-run; gather never counts as "the plan you're reviewing."

## One-shot commands

```bash
# Normal: plan + run (prompts to approve risky/destructive steps; Ctrl-C interrupts)
flux run "rename every TODO comment in src/ to FIXME"
# (every entry point is a subcommand: a bare `flux rename …` is a clap "unrecognized subcommand"
#  error, so a stray word never starts the agent — use `flux run <prompt>`)

# Run unattended (auto-approve every step — for headless/trusted use)
flux run --yes "delete the *.tmp files in build/"

# Plan mode: show the plan, then (on a terminal) ask "run it? [y/N]"
flux plan "summarize README.md into SUMMARY.txt"

# Inspect the plan as data — prints the graph and exits, never runs
flux plan -o json "print hello world 3 times"
flux plan -o yaml "..."       # yaml | json | pretty (default)
```

`flux plan` prints-and-exits whenever output is piped or `-o json|yaml` is given (so it's safe in
scripts); on an interactive terminal with no `-o`, it shows the plan and offers to run it.

## Interactive session (REPL)

```bash
flux                 # start a REPL (normal mode)
flux run -c          # continue the most recent session
```

Inside the REPL:

| Command | Effect |
|---|---|
| `/plan` | toggle **plan mode** (the prompt shows `plan ›`); turns show a plan but don't run it |
| `/run` | execute the plan you just reviewed |
| `/model <spec>` | switch model (e.g. `/model opus`) |
| `/tools` | list available operations |
| `/sessions`, `/resume <id>`, `/clear` | session management |
| `/help` | full command list |

A plan-mode session looks like: type a task → see the plan → either `/run` it, or **just keep typing to
refine it** ("make it also back up the file first") and a new plan appears. `/plan` again returns to
normal mode.

## Approval & safety

Every operation — whether from a one-shot prompt, a `/run`, or a normal turn — goes through the same
envelope:

- **Reads** are pre-allowed; they run without prompting.
- **Writes / commands** prompt for approval unless you pass `--yes` or have an allow-rule in
  `.flux/config.toml`.
- **Destructive** operations (`rm -rf`, force-push, `mkfs`, …) escalate to their own confirmation,
  with two deliberate exceptions: **`--yes` auto-approves everything, destructive steps included**
  (it installs a headless allow-all approver — that is what unattended means), and a destructive step
  **already disclosed in the plan preview you approved** does not re-prompt (approving the rendered
  plan *was* the confirmation; a destructive command assembled at runtime, invisible to the preview,
  still escalates).
- Secrets are redacted from tool output and logs.

Approve a prompt with `y` (once), `a` (always — saved to `.flux/config.toml`), or `N` (deny).

## Models & providers

```bash
flux run -m opus "..."                   # Anthropic alias: opus | sonnet | haiku
flux run -m openai/gpt-5 "..."           # provider/model
flux run -m openrouter/anthropic/claude-... "..."
flux auth status                         # which providers are configured
flux auth login claude                   # Claude subscription (OAuth)
```

Default model is `sonnet`, overridable in `.flux/config.toml` (`model = "..."`) or per-call with `-m`.

**Sub-agent role `model:` overrides** (`.flux/agents/<role>.md` frontmatter) speak the same spec
form as `-m` above, but with one constraint: **a sub-agent always runs on its parent's provider** —
there is no per-sub-agent provider factory. So a role's `model:` value may be:
- a bare model id (no provider prefix), or a spec prefixed by the **parent's own** provider — either
  way it resolves to what the parent's provider expects (a spec like
  `openrouter/deepseek/deepseek-v4-flash` under an `openrouter` parent has the `openrouter/` prefix
  stripped before it reaches the wire);
- but **not** a spec naming a *different* provider than the parent's — that fails fast at spawn time
  with a diagnostic naming both providers, rather than reaching the wire and failing mid-turn.

Omit `model:` (or leave it blank) to inherit the parent's model outright.

## Configuration (`.flux/config.toml`)

```toml
model = "sonnet"

[permissions]
allow = ["read", "glob", "grep", "search"]   # auto-approved tools (reads are the default)
deny  = []                                    # always-blocked tools
```

## Other surfaces

```bash
flux run -v "..."                # show tool output in full (no truncation); also FLUX_VERBOSE=1
flux --color always|auto|never   # colorize output (auto = a terminal, NO_COLOR unset; global flag)
flux tui                         # ratatui chat UI (in-UI approval modal)
flux app run --serve 127.0.0.1:8787 --yes  # HTTP/A2A daemon (REST + SSE)
flux run app.flux                # run a multi-agent program (event bus + triggers + journeys); deny-destructive unless --yes
flux flow run <file.flux>        # run one checked-in Flux-Lang flow directly (native text or DraftAst JSON),
                                 #   skipping NL→plan; opt-in resumable mode (L-25):
                                 #   --resumable          a halt (a failed statement, or a paused `await`) prints
                                 #                        a structured halt report (✓/✗/· marked statements,
                                 #                        machine-readable failure, session id) and exits non-zero
                                 #                        instead of erroring the whole run
                                 #   --resume <session>   re-parse the (corrected) file, fold that session's
                                 #     | --resume last     halt ledger, fast-forward the matching completed prefix
                                 #                        (values rehydrated), and execute from the first changed
                                 #                        statement; `last` needs the flow to declare a name
                                 #                        (`flow <name> -> …`) to find its session unambiguously
flux sessions                    # list recent sessions
flux usage                       # aligned token/cost dashboard for flux + detected Codex,
                                 #   Claude Code, and opencode stores; use --no-external for
                                 #   flux-only, --harness flux,codex,claude,opencode to filter,
                                 #   or --json for normalized machine-readable rows
flux replay <session|last>       # TIME MACHINE (C-43/A-45): hermetically re-execute a recorded run —
                                 #   plans re-parse from the durable plan_source, op outputs are served
                                 #   from the recorded cassette: NO model call, NO live IO, side effects
                                 #   never re-fire; transcript renders like the original minus latency.
                                 #   --turn N · --sub-agents (replay the A-08 child streams too) ·
                                 #   --json; exit 1 if the replay diverges from the recording.
                                 #   Capture is on by default (per-op cap FLUX_CASSETTE_MAX_BYTES,
                                 #   1 MiB); disable with FLUX_CASSETTE=0 — then nothing is replayable.
flux fork <session> --at N       # TIME MACHINE (A-46): branch a recorded run at top-level statement N
                                 #   of its final plan — the prefix replays from tape (no side effects),
                                 #   the tail diverges LIVE through the real approval envelope:
                                 #   --inject '<json>'    bind a different value there, run the rest
                                 #   --edit <file.flux>   continue with a corrected plan (unchanged
                                 #                        statements fast-forward, edits run live)
                                 #   (default) --replan   let the model re-plan from the forked state
                                 #   The forked session records its own cassette → replayable/diffable.
flux diff <A> <B>                # TIME MACHINE (C-44): align two recorded runs; shows where the PLAN
                                 #   changed vs where the same plan hit a DIFFERENT WORLD (op output
                                 #   differs); --json; exit 1 when the runs diverge (diff-style)
flux plugin install <name>       # the plugin CLI — verified install from the signed plugin pack (@<version>, --all;
                                 #   --dir registers local builds); also ls / status / call / pin / rollback / uninstall / skill
flux eval synthetic --watch      # run a benchmark suite (synthetic riddles / mock / terminal-bench / multi);
                                 #   --watch streams the agent live, --report out.md writes a categorized report
```

A **multi-agent program** is a **native flux-lang `.flux` file** that declares the whole app as typed
module declarations — `agent` / `channel` / `datasource` / `trigger` / `journey` — with each module's
settings written inline as flux-lang values, and secrets as `secret "ENV_NAME"` *references* (resolved
from the environment at load; plaintext is never inline). Journey bodies are ordinary flux-lang flows.
See `crates/flux-app/examples/hello.flux` (minimal) and `crates/flux-app/examples/support-bot.flux`
(the full agent + Slack channel + datasource surface), and
[`designs/native-text-modules.md`](designs/native-text-modules.md). To **embed** flux as a library, use
`flux-sdk`'s `FlowClient` for the Flux-Lang compile→analyze→execute lifecycle (`crates/flux-sdk/src/flow.rs`).

Plans and tool *inputs* always print in full; tool *output* (e.g. a large file read) is previewed by
default and shown in full with `-v`.

## Tips

- **Use `flux plan` (or the REPL `/plan` toggle) first** when a task is risky or you want to review the
  approach — then run it once you're happy.
- **Plan mode auto-runs a bounded read-only gather pass, then shows (and never auto-runs) the final
  plan:** a self-contained task ("delete the .tmp files", "print 3×") settles on the full plan
  immediately, same as before — no added latency. A task that needs to look around first ("refactor
  the biggest file in src/") gets to read/grep/list on its own before proposing the real plan, instead
  of guessing blind; what runs automatically is always read-only (compile-time enforced), and what
  changes anything is always the plan you review and approve.
- Pass `--yes` only when you trust the task to run unattended: it auto-approves **every** step,
  destructive ones included — there is no re-confirmation under `--yes`. Without it, destructive
  steps get their own prompt.
