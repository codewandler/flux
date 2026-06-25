# Using flux

flux is a coding/ops agent with one core idea: **the LLM is not the runtime.** Instead of the model
calling tools live, one step at a time, the model is a *compiler front-end* — it turns your request into
a typed **execution plan** (a small Flux-Lang graph), and a deterministic Rust runtime executes that
plan through a safety envelope. You always see the plan before it runs, and the same plan can be re-run.

This page is the practical guide. For the design, see [`docs/designs/flux-flow.md`](designs/flux-flow.md).

## The mental model

Every turn, the model does exactly one of two things:

- **emits a plan** — a graph of operations (`read`, `bash`, `edit`, `repeat`, `when`, …), or
- **answers in prose** — when no operation is needed.

The built-in file operations are: `read` (raw text, with a line-numbered view; refuses binary and
guides you to a range for very large files), `read_many` (survey several files in one node), `write`
(create/overwrite, returns a diff), `append` (lower-risk add to a file), `edit` (string replace with
progressively looser whitespace/indentation matching and a unified diff), `patch` (line-anchored
`insert_before/after`/`replace_range`/`delete_range`), `glob`, and `grep` (regex by default; pass
`literal` for a plain substring). A file must be **read before you `edit`/`patch` it** — and if it
changed on disk since you read it, the edit is refused so you re-read first.

It has **no other tools.** It can't call `bash` or `read` directly; even reading a file is a node in a
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

## Two modes: normal and plan

| Mode | What a turn does |
|---|---|
| **normal** (default) | the model plans → the runtime **shows the plan, then runs it** (risky steps prompt for approval) |
| **plan** | the model plans → the runtime **shows the plan but does NOT run it**; you review/refine, then approve to run |

Plan mode is for "let me see (and shape) the whole plan before anything happens." Normal mode just does
the work, gating risky steps as they come.

## One-shot commands

```bash
# Normal: plan + run (prompts to approve risky/destructive steps; Ctrl-C interrupts)
flux "rename every TODO comment in src/ to FIXME"

# Run unattended (auto-approve every step — for headless/trusted use)
flux --yes "delete the *.tmp files in build/"

# Plan mode: show the plan, then (on a terminal) ask "run it? [y/N]"
flux --plan "summarize README.md into SUMMARY.txt"

# Inspect the plan as data — prints the graph and exits, never runs
flux --plan -o json "print hello world 3 times"
flux --plan -o yaml "..."     # yaml | json | pretty (default)
```

`--plan` prints-and-exits whenever output is piped or `-o json|yaml` is given (so it's safe in scripts);
on an interactive terminal with no `-o`, it shows the plan and offers to run it.

## Interactive session (REPL)

```bash
flux                 # start a REPL (normal mode)
flux -c              # continue the most recent session
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
- **Destructive** operations (`rm -rf`, force-push, `mkfs`, …) **always** re-confirm — even with `--yes`
  off they prompt, and even inside an approved plan they escalate. This can't be bypassed by the plan.
- Secrets are redacted from tool output and logs.

Approve a prompt with `y` (once), `a` (always — saved to `.flux/config.toml`), or `N` (deny).

## Models & providers

```bash
flux -m opus "..."                       # Anthropic alias: opus | sonnet | haiku
flux -m openai/gpt-5 "..."               # provider/model
flux -m openrouter/anthropic/claude-... "..."
flux auth status                         # which providers are configured
flux auth login claude                   # Claude subscription (OAuth)
```

Default model is `sonnet`, overridable in `.flux/config.toml` (`model = "..."`) or per-call with `-m`.

## Configuration (`.flux/config.toml`)

```toml
model = "sonnet"

[permissions]
allow = ["read", "glob", "grep", "search"]   # auto-approved tools (reads are the default)
deny  = []                                    # always-blocked tools
```

## Other surfaces

```bash
flux -v "..."                    # show tool output in full (no truncation); also FLUX_VERBOSE=1
flux --color always|auto|never   # colorize output (auto = a terminal, NO_COLOR unset)
flux --tui                       # ratatui chat UI (in-UI approval modal)
flux --serve 127.0.0.1:8787 --yes   # HTTP API daemon (REST + SSE)
flux sessions                    # list recent sessions
flux plugin ls                   # manage subprocess plugins (any-language ops)
```

Plans and tool *inputs* always print in full; tool *output* (e.g. a large file read) is previewed by
default and shown in full with `-v`.

## Tips

- **Use `--plan` (or `/plan`) first** when a task is risky or you want to review the approach — then run
  it once you're happy.
- **Plan mode is single-shot per turn:** great for self-contained tasks ("delete the .tmp files",
  "print 3×"). For exploratory work that needs to read a file *before* deciding what to do, use **normal
  mode** — it reads, sees the result, and plans the next step automatically.
- Pass `--yes` only when you trust the task to run unattended; destructive steps still re-confirm.
