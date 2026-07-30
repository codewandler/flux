# Using flux

flux is a coding/ops agent with one core idea: **the LLM is not the runtime.** The model interprets
intent, explores through the exact native schemas of a narrow capability set, and proposes literal
operation calls. An authored Flux-Lang outer loop owns order, bounds, questions, approval, and
execution; the model never emits executable Flux.

This page is the practical guide. For the design, see [`docs/designs/flux-flow.md`](designs/flux-flow.md).

## The mental model

The default turn has explicit phases:

1. A typed intent stage chooses the smallest semantic capability families.
2. The host intersects those signals with registered, wired, permitted operations.
3. A native exploration stage uses the real operation schemas. Safe reads execute through the
   envelope and become evidence; effectful calls are captured rather than executed.
4. The host freezes captured calls into an immutable action batch and asks once for approval.
5. A matching one-shot receipt lets the runtime execute each action through the usual envelope.
6. The same native ledger receives the execution report and repairs failed work locally or presents
   the result.

The built-in file operations are: `read` (one file, a list of files, or a glob pattern — single-file
reads get a line-numbered view, multi-file reads return `==> path <==` sections; refuses binary and
guides you to a range for very large files), `read_many` (a first-class bulk read — once several
relevant paths are known, the registry tells the model to prefer it over sequential `read` calls;
each section is headed `==> path <==`), `write`
(create/overwrite, returns a diff), `append` (lower-risk add to a file), `edit` (string replace with
progressively looser whitespace/indentation matching and a unified diff), `patch` (line-anchored
`insert_before/after`/`replace_range`/`delete_range`), `glob`, and `grep` (regex by default; pass
`literal` for a plain substring). A file must be **read before you `edit`/`patch` it** — and if it
changed on disk since you read it, the edit is refused so you re-read first. A generic `bash` op
exists but is **off by default** (the `shell` tool group): opt in with `enable_shell = true` in
`.flux/config.toml` or `FLUX_ENABLE_BASH=1` — prefer the dedicated ops.

Operation metadata—not model output—decides whether a call is gather-safe. A mutating, destructive,
opaque, or non-idempotent operation can never be relabeled as a read. Every call still traverses
permissions, approval, redaction, and guarded IO. See [`docs/agent-loop.md`](agent-loop.md).

## One-shot commands

```bash
# Run the adaptive loop (prompts once for a proposed effect batch; Ctrl-C interrupts)
flux run "rename every TODO comment in src/ to FIXME"
# (every entry point is a subcommand: a bare `flux rename …` is a clap "unrecognized subcommand"
#  error, so a stray word never starts the agent — use `flux run <prompt>`)

# Run unattended (auto-approve every step — for headless/trusted use)
flux run --yes "delete the *.tmp files in build/"

# Include typed stage and batch machinery in the stream
flux run --show-loop "summarize README.md into SUMMARY.txt"

# Select an explicit authored outer loop (the default is `adaptive`)
flux run --loop loops/support.flux "triage this request"
```

## Interactive session (REPL)

```bash
flux                 # start a REPL (normal mode)
flux run -c          # continue the most recent session
```

Inside the REPL:

| Command | Effect |
|---|---|
| `/model <spec>` | switch model (e.g. `/model opus`) |
| `/tools` | list available operations |
| `/evidence` | inspect intent, tool, approval, and execution observations |
| `/sessions`, `/resume <id>`, `/clear` | session management |
| `/help` | full command list |

## Terminal UI

```bash
flux tui                 # dense full-screen chat
flux tui -c              # continue the newest session
flux tui -m mock         # exercise the full UI offline
```

The TUI keeps the transcript borderless and separates its multiline composer only with a quiet
background. Enter sends; `Ctrl-J`, `Alt-Enter`, or `Shift-Enter` inserts a newline. Bracketed paste is
inserted atomically. While a turn runs, Enter adds a visible FIFO follow-up instead of replacing the
previous one; `/queue` opens the editor (`Delete`, `Alt-Up`/`Alt-Down`, Enter to edit).

`/model`, `/shell`, `/tools`, `/evidence`, `/compact`, `/new`, and `/clear` mirror
the REPL controls. `/sessions` opens a picker, and `/resume <id>` switches directly; either path
reconstructs messages, plans, tool results, notices, and usage from the durable session log without
re-running operations. Use PgUp/PgDn or the mouse wheel for scrollback and `Ctrl-End` to follow the
latest activity. `Ctrl-E` expands thinking/tool details, `Ctrl-C` interrupts a turn, and `Ctrl-D` or
`/quit` exits. Approvals use `y` once, `a` always, and any other key to deny.

## Approval & safety

Every operation—whether it is an exploration read, an approved batch action, or an authored-flow
call—goes through the same envelope:

- **Authorization is the floor.** Each concrete call is checked against its typed workspace,
  datasource, host, provider, network, connection, process, secret, or semantic-action requirements.
  The plan preview and dispatch use the same requirements; a denial stops before approval or IO.
- **Reads** allowed by the authorization profile are pre-approved by the default permission rules;
  they run without prompting.
- **Writes / commands** prompt for approval unless you pass `--yes` or have an allow-rule in
  `.flux/config.toml`.
- **Destructive** operations (`rm -rf`, force-push, `mkfs`, …) are disclosed in the aggregate batch
  approval. `--yes` installs a headless allow-all approver for trusted unattended work, but it never
  overrides an authorization-policy denial.
- Secrets are redacted from tool output and logs.

Approve a prompt with `y` (once), `a` (always — saved to `.flux/config.toml`), or `N` (deny).

## Models & providers

```bash
flux run -m opus "..."                   # Anthropic alias: opus | sonnet | haiku
flux run -m openai/gpt-5 "..."           # provider/model
flux run -m openrouter/anthropic/claude-... "..."
flux auth status                         # which providers are configured
flux auth login claude                   # Claude subscription (OAuth)
flux auth login codex                    # OpenAI/codex subscription (PKCE OAuth) — also the fix
                                         #   when a stored codex login expires (re-mints the token)
flux auth set slack bot_token            # store a plugin bearer token (~/.flux/credentials.toml,
                                         #   0600) so sessions resolve it WITHOUT the env var;
                                         #   prompts hidden, or pipe it in; --clear removes it
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
flux tui                         # dense ratatui chat UI (queue, session replay, approval sheet)
flux app run --serve 127.0.0.1:8787 --yes  # HTTP/A2A daemon (REST + SSE)
flux run app.flux                # run a multi-agent program (event bus + triggers + journeys); deny-destructive unless --yes
flux run workflows.flux --entry triage --arg queue=new
                                 # select one named top-level flow from a multi-flow module and exit;
                                 # --inputs JSON and repeatable --arg use the strict flow input contract
flux a2a <url> [prompt…]         # connect to a remote A2A agent and chat like a local one (the client
                                 #   side of `app run --serve`): with a prompt or piped stdin it runs
                                 #   one turn and exits, otherwise it opens a REPL. --token <t> for a
                                 #   gated endpoint (falls back to FLUX_A2A_TOKEN)
flux flow list                   # list saved flows + composite ops (alias: `flux flow ls`) from
                                 #   .flux/flows first, then ~/.flux/flows; no agent session/model
flux flow run <name|file>        # run a saved flow by filename stem/declared name, or an existing
                                 #   file (files win; native text or DraftAst JSON), skipping NL→plan
                                 #   --inputs '{"env":"dev"}'     typed JSON-object inputs
                                 #   --arg env=dev --arg replicas=3  repeatable typed overrides;
                                 #                                    later duplicate args win
                                 #   --map-inputs "three in dev"    opt-in model mapping for only
                                 #                                    the still-missing parameters
                                 #   Declared flow parameters are required. Unknown/missing keys,
                                 #   malformed JSON, and concrete type mismatches fail before effects.
                                 #   Without --map-inputs this path is deterministic and needs no
                                 #   provider credentials unless the flow itself contains model ops.
                                 #   Existing opt-in resumable mode (L-25):
                                 #   --resumable          a halt (a failed statement, or a paused `await`) prints
                                 #                        a structured halt report (✓/✗/· marked statements,
                                 #                        machine-readable failure, session id) and exits non-zero
                                 #                        instead of erroring the whole run
                                 #   --resume <session>   re-parse the (corrected) file, fold that session's
                                 #     | --resume last     halt ledger, fast-forward the matching completed prefix
                                 #                        (values rehydrated), and execute from the first changed
                                 #                        statement; `last` needs the flow to declare a name
                                 #                        (`flow <name> -> …`) to find its session unambiguously
flux catalog core --format json # export versioned core operations, language nodes, capabilities,
                                 #   and their schemas; deterministic, offline, and no operation runs
flux render <file.flux>          # render a .flux file as a syntax-highlighted image (One Dark):
                                 #   --view source (default) | tree; -o out.svg writes SVG, -o
                                 #   out.png rasterizes PNG (embedded font); both workspace-
                                 #   confined, stdout (SVG) otherwise; the doc-image generator
flux sessions                    # list recent sessions
flux wakeups list [<session>]    # inspect agent-scheduled wake-ups (A-98) — a turn that called
                                 #   `schedule_wakeup` to resume itself later. `list` is the
                                 #   default action and the session defaults to `last`;
                                 #   `cancel <session> <wakeup-id>` cancels one before it fires
flux usage                       # aligned token/cost dashboard for flux + detected Codex,
                                 #   Claude Code, and opencode stores. Shows period/session/time
                                 #   metrics, a per-harness + absolute total summary, cache share,
                                 #   priced/unpriced rows, and TTY scan progress for large local
                                 #   histories. Use --last 7d,
                                 #   --since/--until, --no-external, --harness ..., --progress ...,
                                 #   or --json for normalized machine-readable metrics + rows
flux replay <session|last>       # TIME MACHINE (C-43/A-45): hermetically re-execute a recorded run —
                                 #   authored or host-derived flows re-parse from durable source, op outputs are served
                                 #   from the recorded cassette: NO model call, NO live IO, side effects
                                 #   never re-fire; transcript renders like the original minus latency.
                                 #   --turn N · --sub-agents (replay the A-08 child streams too) ·
                                 #   --json; exit 1 if the replay diverges from the recording.
                                 #   Capture is on by default (per-op cap FLUX_CASSETTE_MAX_BYTES,
                                 #   1 MiB); disable with FLUX_CASSETTE=0 — then nothing is replayable.
flux fork <session> --at N       # TIME MACHINE (A-46): branch a recorded run at top-level statement N
                                 #   of a recorded authored/host-derived flow — prefix from tape (no side effects),
                                 #   the tail diverges LIVE through the real approval envelope:
                                 #   --inject '<json>'    bind a different value there, run the rest
                                 #   --edit <file.flux>   continue with a corrected plan (unchanged
                                 #                        statements fast-forward, edits run live)
                                 #   (default) --replan   continue adaptively from the forked state
                                 #   The forked session records its own cassette → replayable/diffable.
flux diff <A> <B>                # TIME MACHINE (C-44): align two recorded runs; shows where the FLOW
                                 #   changed vs where the same flow hit a DIFFERENT WORLD (op output
                                 #   differs); --json; exit 1 when the runs diverge (diff-style)
flux export <run> -o run.html    # TIME MACHINE (C-132): render a recorded run as ONE self-contained
                                 #   static HTML file — plan tree (the `flow_render` substrate),
                                 #   per-op results and diffs, cost, timeline, sub-agent children
                                 #   nested, every rendered string redacted (C-22). The read-only
                                 #   sibling of replay/fork/diff: a pure read, no event-store write,
                                 #   no provider. Inline CSS, no JS, no network refs. <run> defaults
                                 #   to `last`; without -o the HTML goes to stdout
flux record <name> "<prompt>"    # record ONE live turn as a committed-safe scenario fixture (D-174):
                                 #   the run's events, flow state, redacted model cassette, and
                                 #   canonical plan snapshot land in tests/scenarios/<name>/ (--dir
                                 #   relocates the fixture root)
flux test [<name>]               # replay those fixtures offline as a test gate (D-174): the REAL
                                 #   agent re-runs against the recorded world under a deny-all
                                 #   approver and a never-called provider — $0, no key, no network.
                                 #   Omit <name> to run every fixture under --dir. Exit 1 if any
                                 #   fixture diverges (prints the plan source and the world diff),
                                 #   so it works as a CI gate; --json for a machine-readable report
flux plugin install <name>       # the plugin CLI — verified install from the signed plugin pack (@<version>, --all;
                                 #   --dir registers local builds); also ls / status / call / pin / rollback / uninstall / skill
flux eval synthetic --watch      # run a benchmark suite (synthetic riddles / mock / terminal-bench / multi);
                                 #   --watch streams the agent live, --report out.md writes a categorized report
flux review --files a.rs b.rs    # run the embedded strict-review protocol over the files and print a
                                 #   ReviewReport (immutable built-in toolless roles; project role files
                                 #   cannot replace them; fail-closed unattended sandbox; stdout only);
                                 #   --format md|json, --fail-on info|low|medium|high|critical (exit 1
                                 #   at/above that severity), -m <spec> reviewer model, --max-tokens N
flux loop show                   # print the built-in adaptive loop. `flux loop eject` writes it to
                                 #   .flux/agent-loop.flux (-f/--force overwrites); the file is inert
                                 #   until selected via `flux run --loop …` or `[agent] loop = "…"`
flux endpoint list               # inspect the persisted endpoint store (~/.flux/endpoints.toml);
                                 #   operator-only, reference-only — never prints a secret value. Also:
                                 #   add <id> --url <bare-url> [--product/--protocol/--credential-ref/
                                 #   --label] · show <id> · resolve <id> (what a ref WOULD bind to) ·
                                 #   import <id> [--from-json <ref>]
flux policy simulate p.toml      # POLICY SIMULATION (C-131): replay a proposed authorization policy over
                                 #   the recorded op history and print a diff — newly blocked / newly
                                 #   allowed / unchanged, with the deciding requirement per op. A pure
                                 #   read: no event is appended and no provider is built (it does create
                                 #   the store dir on a fresh HOME, as `export`/`sessions` do).
                                 #   Ops the log cannot re-evaluate are reported "indeterminate" with a
                                 #   reason — never folded into blocked or allowed: no authority contract
                                 #   in this build, a malformed record, a verdict that turns on caller
                                 #   trust/scopes/groups (bracketed jointly, so grants gated on several
                                 #   at once are caught), or one that turns on the caller's principal
                                 #   kind on a record predating `caller_kind`.
                                 #   Only the mandatory policy floor is replayed — not permission rules,
                                 #   the capability-scope floor, `[tools] disable`, or the approval gate.
                                 #   `newly allowed` shows approval_required->allow only: a denied
                                 #   dispatch is never recorded, so deny->allow cannot appear.
                                 #   --sessions N limits the replay window (0 = all), --json for tooling
flux skill [cli|lang|plugin|ops] # print a generated Claude-format skill's SKILL.md to stdout (omit the
                                 #   type for the root skill); --install writes skill directories to
                                 #   .flux/skills instead, --global targets ~/.claude/skills
flux changelog                   # show what changed in flux, in plain language (the customer changelog);
                                 #   [<version>] one section, --all every release, --unreleased the
                                 #   not-yet-released (development) section
flux completion [shell]          # print a shell completion script to stdout (defaults to fish); an
                                 #   unknown shell is a usage error (exit 2)
flux doctor                      # diagnose a flux install end-to-end (C-128): provider credentials
                                 #   (incl. OAuth expiry), plugin-pack signature/hash drift, the OS
                                 #   sandbox backend, events.db health, private-network egress
                                 #   config sanity, `[tools] disable` resolution, and version skew
                                 #   vs the latest release — each non-pass carries a one-line
                                 #   fix-it hint. Exit is non-zero iff a check FAILS (a warning
                                 #   never fails the run); --json for scripting
flux preset list                 # the recipe cookbook — scaffold or run a parameterized flow. `help
                                 #   <name>` shows a preset's keys; `<name> key=value …` scaffolds it
                                 #   (-o pretty|json), add --run [--yes] [-m <spec>] to execute instead
```

A **multi-agent program** is a **native flux-lang `.flux` file** that declares the whole app as typed
module declarations — `agent_loop` / `agent` / `channel` / `datasource` / `trigger` / `journey` — with each module's
settings written inline as flux-lang values, and secrets as `secret "ENV_NAME"` *references* (resolved
from the environment at load; plaintext is never inline). Journey bodies are ordinary flux-lang flows.
See `crates/flux-app/examples/hello.flux` (minimal) and `crates/flux-app/examples/support-bot.flux`
(the full agent + Slack channel + datasource surface), and
[`designs/native-text-modules.md`](designs/native-text-modules.md). To **embed** flux as a library, use
`flux-sdk`'s `FlowClient` for the Flux-Lang parse/construct → analyze → execute lifecycle (`crates/flux-sdk/src/flow.rs`).

Action batches and operation *inputs* print in full; operation *output* (e.g. a large file read) is previewed by
default and shown in full with `-v`.

## Tips

- Use `--show-loop` when diagnosing latency or capability selection. It reveals intent, exploration,
  approval, execution, and presentation stages without changing their behavior.
- Reads needed for evidence may run during exploration; writes and other effects cannot. They are
  frozen into the batch shown at the approval boundary.
- Use an authored flow or custom `--loop` when an invariant must hold structurally—for example,
  “search the handbook before every answer”—rather than depending on prompt compliance.
- Pass `--yes` only when you trust the task to run unattended: it auto-approves **every** step,
  destructive ones included — there is no re-confirmation under `--yes`. Without it, destructive
  steps get their own prompt.
