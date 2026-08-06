---
name: flux-tui
description: "Operate and beta-test the installed Flux terminal UI through tmux, especially `flux tui --fleet`: install a local build, start or restart the `flux:fleet` pane, send literal composer input, capture and inspect output, verify colors, distinguish tmux scrollback from durable Flux session history, and reproduce catalog/approval/Fleet-main defects. Use for Flux TUI development, live Fleet coordinator testing, tmux-driven regressions, or requests to observe the running TUI after a code change."
---

# Flux TUI

Drive the installed binary as an operator would. Treat tmux as the terminal/composer and observation
surface; use typed Board/Fleet operations or CLI services for lifecycle state, never direct edits to
`.flux/fleet/state.json` or the event journal.

## Establish the target

Resolve the two roots explicitly:

- Flux source/install root: usually `/home/timo/projects/flux`
- Fleet root: usually `/home/timo/projects/flux-roadmap`
- tmux target: `flux:fleet` (session `flux`, window `fleet`)

Inspect before changing the running process:

```bash
git -C /home/timo/projects/flux status --short --branch
tmux list-panes -t flux:fleet -F '#{session_name}:#{window_name}.#{pane_index} pid=#{pane_pid} current=#{pane_current_command} dead=#{pane_dead} path=#{pane_current_path}'
tmux capture-pane -p -t flux:fleet -S -120
```

Preserve dirty source changes and durable Fleet history. Do not kill the tmux session merely to
restart Flux.

## Install, restart, observe

A harness change is not done until it is installed and running: workers execute the *installed*
binary, so editing Rust changes nothing about the Fleet under test until you reinstall.

**Stop the TUI before installing, not after.** `cargo install` replaces the binary by rename, so every
already-running `flux` process then reads `/proc/self/exe` as `".../flux (deleted)"`. Fleet resolves
`std::env::current_exe()` at call time, not at startup, so a TUI left running across an install keeps
rendering normally while every nested `flux fleet …` call and every worker spawn it attempts fails
with ENOENT — surfacing as `transient-worker: agent {id} could not start`, a worker failure that is
really a stale-binary artifact.

After each coherent code change:

1. Check for an in-flight wave: `flux fleet status`, and
   `pgrep -af 'bin/flux .*run --stream-json'` (match the absolute path — worker argv starts with the
   resolved executable, so a bare `flux run` pattern silently matches nothing).
2. If a wave is running, `flux fleet cancel <wave>`. Cancel is a ~50 ms SIGKILL of the worker process
   group, so first **snapshot every story worktree** — `git log origin/main..HEAD` *and* `git diff`.
   A worker recorded `failed` can still hold a complete commit.
3. Stop the TUI: `tmux respawn-pane -k -t flux:fleet -c <fleet root> 'fish'`. Confirm no
   `flux tui` process survives.
4. Run `task install` from the Flux source root (it runs `cargo test --workspace --lib` first). If it
   fails, inspect and fix it; do not restart onto an unverified binary.
5. Respawn the pane on the installed command:

```bash
tmux respawn-pane -k -t flux:fleet -c /home/timo/projects/flux-roadmap \
  'env -u NO_COLOR COLORTERM=truecolor flux tui -m claude/opus --yes --fleet'
```

6. Confirm the PID/current command changed, capture startup output, then re-dispatch the cancelled
   items.

Name the model provider explicitly. `claude/opus` routes through the Claude subscription OAuth
imported from `~/.claude/.credentials.json`; the bare alias `opus` resolves to `anthropic/opus` and
bills the `ANTHROPIC_API_KEY`, which has no credit balance and fails the turn with HTTP 400 before
any tool runs. Keep the TUI model and `.flux/fleet.toml`'s `[main]`/`[[agent_templates]]` models on
the same provider prefix — a sub-agent role naming a different provider than its parent fails fast at
spawn. Check `flux auth status` when a provider looks exhausted; a 429 `usage_limit_reached` is a
quota fact to route around, not a defect to patch.

Create the target only when it is genuinely absent:

```bash
tmux has-session -t flux 2>/dev/null || \
  tmux new-session -d -s flux -n fleet -c /home/timo/projects/flux-roadmap
```

If session `flux` exists but window `fleet` does not, create that window rather than another session.

## Send a turn safely

Send text literally, then send Enter separately. This avoids shell interpretation and makes the
composer boundary explicit:

```bash
tmux send-keys -t flux:fleet -l -- 'List the durable Fleet workers and summarize their statuses.'
tmux send-keys -t flux:fleet Enter
```

Poll with bounded captures while the turn runs:

```bash
tmux capture-pane -p -t flux:fleet -S -200
```

Do not scrape the terminal as an automation API. Captures are beta-test evidence; acknowledged
`board.*`/`fleet.*` results and CLI JSON remain the product contract.

## Verify the Fleet-main boundary

Use `/tools` or a direct free-form request and inspect the captured output. For an attached native
Fleet main, verify:

- only native Board management, native Fleet management, bounded research `task`, and hidden loop
  machinery are reachable;
- shell, filesystem editing, git mutation, web/plugin/eval/pane operations and legacy transient
  process-worker `fleet.*` operations are absent;
- `fleet.agents`, Board reads, Fleet status and Fleet schedule do not request approval;
- effectful dispatch or mutation follows its declared confirmation/revision contract;
- a free-form question uses the configured authored coordinator loop and does not surface the
  generic adaptive intent/history-budget path;
- research children remain read-only and cannot inherit coordinator mutation authority.

When an unexpected approval, missing operation, stale worker state, or general-purpose tool appears,
capture the exact turn and record it in the owning story before patching.

## Diagnose colors

The running pane can inherit `NO_COLOR` from the tmux server even when the current shell no longer
has it. Inspect both the server and pane:

```bash
tmux show-environment -g NO_COLOR
tmux display-message -p -t flux:fleet 'TERM=#{client_termname} pane=#{pane_id} command=#{pane_current_command}'
tmux show-options -g default-terminal
tmux capture-pane -p -e -t flux:fleet -S -40
```

Prefer the per-process `env -u NO_COLOR COLORTERM=truecolor` restart above. `--yes` keeps unattended
beta observation from stalling on an approval modal; still treat any unexpected operation proposal
as a product defect rather than evidence that its authority is appropriate. Do not globally rewrite a
user's tmux configuration without an explicit request. Note that plain `capture-pane -p` normally
omits styling; use `-e` when checking escape sequences, and inspect the visible pane for final proof.

## Explain retained history correctly

Three different histories exist:

- tmux scrollback: clear only with `tmux clear-history -t flux:fleet`;
- the TUI's rendered transcript: reconstructed from the Flux session;
- durable Fleet-main conversation: tied to `.flux/fleet/state.json`'s recorded main session and
  intentionally resumed by `flux tui --fleet`.

Restarting the process changes the PID but does not mint a new Fleet-main session, so the transcript
returning is expected. Never delete or edit runtime state to make a test look fresh. Use the typed
Fleet/session command designed for that transition, or test the next current turn while explicitly
accounting for resumed history.

## Judge a wave by git, never by turn status

A worker's turn outcome is not a delivery guarantee in either direction, and both failure modes have
been observed in one wave:

- a turn reported **success** having committed nothing (nothing yet requires a worker to signal
  terminal state — decision 0014 §3's lifecycle report operation is unbuilt, story C-570);
- a turn reported **failure** holding a complete commit, because a budget fired *after* the commit
  landed.

So verify each story worktree directly — `git log origin/main..HEAD` and `git status --porcelain`
under `.flux/fleet/worktrees/<wave>/<repo>/stories/<ID>` — before believing a wave summary, and
always before re-dispatching, or you will discard finished work and pay to redo it.

Read worker errors from `.flux/fleet/events.ndjson`. The bounded `flux fleet inspect activity` view
elides them as `$omitted`, so the durable journal is where a failure is actually legible.

## Finish with evidence

Report the installed-build result, old/new pane PID, exact input sent, relevant captured output,
observed operation/approval boundary, and any remaining defect. Keep code verification separate from
visual TUI verification; both are required when behavior and rendering changed.
