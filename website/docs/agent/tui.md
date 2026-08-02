---
title: TUI
description: "The full-screen terminal chat UI: keybindings, the composer, mid-turn steering, transcript search, and in-UI approvals."
---

# TUI

`flux tui` is the full-screen terminal chat UI — the same agent loop, the same safety envelope, and
the same session store as `flux run`, driven interactively. It renders a streaming transcript,
tool cards, plan trees, and an in-UI approval sheet, and it keeps the composer live while a turn is
running so you can write the next message (or steer the current one) without waiting.

```bash
flux tui                       # start in the current workspace
flux tui -m opus               # pick a model for the session
flux tui --yes                 # auto-approve every admitted tool call (no approval sheet)
flux tui -c                    # continue the most recent session
flux tui --remote https://worker.example:8790  # approve here; effects land there
```

`flux tui` takes the same turn-control flags as `flux run` — see [CLI](./cli.md#turn-controls) for
`-m`, `--effort`, `--max-tokens`, `--turn-budget`, `--loop`, `--skill`, and the rest, and
[CLI](./cli.md#global-flags) for the flags that apply to every command.

In remote mode the header continuously shows the endpoint and canonical remote workspace. It is not
a startup notice that scrolls away. The directory where the TUI started remains the local control
plane; flux does not synchronize it with the remote tree.

Press `F1` at any time for the in-app keybinding and slash-command list. That overlay is
generated from the same tables the UI dispatches on, so it can never drift from the running binary.

## Keybindings

Everything not listed below edits the composer — text, `Backspace`, arrows, word
navigation, `Home`/`End`, `Ctrl-U` undo — and stays live while a turn
runs.

| Key | What it does |
|---|---|
| `Enter` | Send. While a turn is running, queue the message instead (see [Queue and steering](#queue-and-steering)). |
| `Ctrl-J` · `Alt-Enter` · `Shift-Enter` | Insert a newline instead of sending. |
| `↑` / `↓` | Recall previous input — only at the composer's first/last row, so they still move the cursor inside a multi-line message. |
| `Ctrl-R` | Reverse history search. Type to match; `Ctrl-R` again steps to an older hit; `Enter` keeps the recalled text in the composer *without* sending; `Esc` restores the draft you started from. |
| `Ctrl-F` | Transcript search. Opens in typing mode; `Enter` leaves typing mode, then `n` / `N` step forward/backward through matches. `Ctrl-F` reopens typing; `Esc` closes. |
| `PgUp` / `PgDn` | Scroll the transcript by a page. `PgDn` re-attaches follow mode when it reaches the bottom. |
| Mouse wheel | Scroll the transcript three lines at a time. |
| `Ctrl-End` | Jump to the latest activity and re-attach follow mode. |
| `Ctrl-G` / `Ctrl-Shift-G` | Jump to the next / previous failed tool card. |
| `Ctrl-E` | Expand/collapse tool and thinking details on **all** cards at once. |
| `Shift-↑` / `Shift-↓` | Move the transcript entry focus. With an entry focused: `Enter` expands/collapses that one card, `y` copies the entry to the system clipboard, `Esc` clears the focus. |
| `Ctrl-T` | Toggle mouse capture. With capture off, your terminal's native select-and-copy works again; wheel scrolling stops, `PgUp`/`PgDn` keep working, and the footer shows `mouse off (Ctrl-T)`. |
| `Ctrl-C` | Context-sensitive: interrupt a running turn · clear a non-empty composer · arm quit when idle and blank (see [Leaving](#leaving)). |
| `Ctrl-D` | Quit — only when the session is idle **and** the composer is empty. |
| `F1` | Open the help overlay (`F1`, `Esc`, `q`, or `Enter` closes it). |
| `Esc` | Dismiss the active popup, cancel a queue edit, or clear a half-typed slash command. |

### Terminal support for the newline keys

`Ctrl-J` always inserts a newline — it is a plain control character every terminal sends.
`Alt-Enter` and `Shift-Enter` only work in terminals that actually report the
modifier on `Enter`. If your terminal sends a bare `Enter` for both, the message
is sent instead; use `Ctrl-J`.

### Pasting

Bracketed paste is enabled for the session, so a multi-line paste arrives as one block and is
inserted into the composer verbatim — newlines inside the pasted text never submit the message.

## The composer

The composer is a full multi-line editor and stays interactive while a turn runs. Two popups can
take over the keys while they have candidates:

**Slash menu.** Typing a bare `/` prefix opens a fuzzy-ranked command menu — prefix beats substring
beats subsequence, so `/thm` finds `/theme`. `↑`/`↓` select, `Tab`
completes the name into the composer (adding a trailing space when the command takes an argument),
`Enter` selects and runs it, `Esc` clears the composer.

**`@` path completion.** An `@`-prefixed token opens a workspace file picker.
`↑`/`↓` select, `Tab` or `Enter` inserts the path,
`Esc` dismisses it for that token. The inventory is a bounded, ignore-aware walk built
lazily on first use and cached for the session: it skips hidden entries, `target/`, and
`node_modules/`, does not follow symlinked directories, and stops at 20,000 files.

## Queue and steering

Pressing `Enter` while a turn is running does not interrupt it. The message goes onto a
**steering queue** shared with the engine, and one of two things happens:

- The adaptive loop drains the queue before the next consultation in its provider-native exploration
  stage and folds the text into the running turn as attributed steering — no cancelled operations,
  no disturbed approval prompt. The transcript records `↪ steering delivered: …`.
- If the turn finishes first, the next queued message starts as its own turn.

Consumption is the commit point: once the engine has drained an item you can no longer edit or
retract it.

`/queue` opens the queue manager over the transcript:

| Key | Effect |
|---|---|
| `↑` / `↓` | Select a queued message |
| `Alt-↑` / `Alt-↓` | Reorder the selected message |
| `Delete` or `Backspace` | Retract the selected message |
| `Enter` | Load it back into the composer to edit (`Enter` re-queues, `Esc` cancels the edit) |
| `Esc` | Close the manager |

## Approvals

When the agent proposes an effectful action batch, the approval sheet takes the keyboard. Only
explicit keys act — every other keystroke is deliberately swallowed, so a stray press can never
silently deny a batch.

| Key | Choice |
|---|---|
| `y` | Allow this call |
| `a` | Allow always — records an allow rule, persisted when the TUI exits |
| `n` or `Esc` | Deny |
| `d` | Deny **with a reason** — type it, `Enter` sends the denial carrying the reason, `Esc` returns to the sheet with the approval still pending |
| `↑` / `↓` | Scroll the sheet's subject list |

`flux tui --yes` skips the sheet for admitted calls and auto-approves them within the active policy,
app, and agent ceilings. See
[Safety and approvals](./safety.md#approving-a-prompt) for what prompts and why.

## Slash commands

These are the TUI's built-ins. A command file discovered from `.flux/commands`, `.claude/commands`,
`~/.flux/commands`, or `~/.claude/commands` is listed alongside them in the menu and in
`F1` — see [Command files](./cli.md#command-files).

| Command | Effect |
|---|---|
| `/help` | Open the keybinding and command overlay (same as `F1`) |
| `/usage` | Overlay: tokens, cache hit rate, and cost for the turn and the session |
| `/model [spec]` | Show the active model, or switch mid-session (`/model opus`) |
| `/effort [low\|medium\|high\|xhigh\|max\|off]` | Show or set reasoning effort; takes effect from the next turn |
| `/theme [name]` | List the palettes and the current one, or switch (see [Themes](#themes)) |
| `/tools` | List the operations registered for this session |
| `/shell` | Toggle the optional `bash` op group from the next turn |
| `/evidence` | Show the session's durable evidence trail |
| `/session` | Show the active session id and model |
| `/sessions` | Open the session picker (`--prune` instead removes empty sessions) |
| `/resume <id>` | Resume a session by id |
| `/new` · `/clear` | Start a fresh session |
| `/compact` | Compact older conversation history now |
| `/queue` | Manage queued follow-ups (see [Queue and steering](#queue-and-steering)) |
| `/quit` · `/exit` | Clear queued follow-ups, cancel a running turn, then leave once cancellation finishes |

While a turn is running, only the read-only commands (`/help`, `/tools`, `/evidence`, `/session`,
`/sessions`, `/queue`, `/theme`, bare `/effort`) and `/quit` run — anything that mutates the session
asks you to interrupt the current action first.

`/sessions` opens a picker rather than printing a list: type to filter, `↑`/`↓` to
select, `Enter` to switch, `Esc` to close. Switching is refused while a turn is
running.

## Themes

`/theme` with no argument lists the palettes and shows the active one. The shipped set is:

`dark` · `light` · `dracula` · `nord` · `high-contrast` · `mono`

Truecolor terminals get a finer-grained tuning of each palette automatically. `NO_COLOR` forces the
`mono` palette regardless of the name you pick.

`/theme <name>` applies the palette immediately and persists it to `~/.flux/config.toml`. It is the
`theme` key documented in the [configuration reference](../reference/config.md#top-level-settings) —
set it there to choose a default without running the command.

The animated intro is skipped under `NO_COLOR`, when `FLUX_NO_SPLASH` is truthy, or in a terminal
too small to draw it.

## Leaving

`Ctrl-C` means different things depending on what is happening:

- **A turn is running** — cancel it. The composer stays live, so you can keep typing.
- **Idle, composer has text** — clear the composer.
- **Idle, composer empty** — *arm* quit. The footer shows `Ctrl-C again to quit`; a second
  `Ctrl-C` within two seconds exits. Any other key disarms it, so a single reflexive press
  never drops you out of the session.

`Ctrl-D` exits directly, but only from an idle session with an empty composer. `/quit` and
`/exit` have one meaning in every state: clear queued follow-ups, cancel a running turn, and leave
once cancellation finishes.

Recorded conversation and accepted-plan progress are durable. The current composer draft and
queued-but-unconsumed follow-ups are not: they live only in the TUI process and are discarded when
you leave. `flux tui -c`, `/resume <id>`, or the `/sessions` picker restores the recorded session. If
that session was killed after accepting a plan, the next entry attempts to finish the durable turn
before running new input; an effect interrupted before its cassette record can run again. See
[Crash recovery](./cli.md#crash-recovery-and-resurrection) for the boundary.

## Related docs

- [CLI](./cli.md) — subcommands, global flags, and the turn controls `flux tui` shares with `flux run`.
- [The agent loop](./agent-loop.md) — what the transcript's stages, plans, and batches actually are.
- [Safety and approvals](./safety.md) — the envelope behind the approval sheet.
- [Configuration](../reference/config.md#top-level-settings) — the `theme` key and the rest of the settings.
- [Providers and models](./providers.md) — how `/model` and `-m` resolve a spec.
- [Troubleshooting](../troubleshooting.md) — credentials, sandbox, and session-state problems.
