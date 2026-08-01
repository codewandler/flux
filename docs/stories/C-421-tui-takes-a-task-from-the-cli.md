---
id: C-421
title: "`flux tui` cannot be given a task — every demo opens with someone typing"
pillar: Core
status: ready
priority: 12
design: docs/designs/session-screencast.md
epic: session-screencast
areas: [flux-cli, flux-tui]
note: "`Commands::Tui` takes only AgentFlags (crates/flux-cli/src/args.rs:298) while `run` and `app run` both take a positional prompt — the TUI is the outlier. Small on its own; it is what makes a demo scriptable, and therefore re-recordable"
---

# The TUI is the one verb you cannot hand a task

## Goal

`flux tui fix the failing test` starts the TUI already working on that task.

## Why it is worth its own story

`Commands::Tui` at `crates/flux-cli/src/args.rs:298` takes only `#[command(flatten)] agent:
AgentFlags` — no prompt. `Commands::Run` at `args.rs:259` takes `prompt: Vec<String>`, and `app run`
does too. The TUI is the outlier, and the gap is felt every time someone demonstrates flux: the
recording opens with a human typing, which is slow, produces typos, and cannot be reproduced.

For the [session-screencast](../designs/session-screencast.md) epic this is the entry point — a run
that starts from a command line is a run that can be **re-recorded** unattended when the theme or the
layout changes. But it stands alone: it is the thing anyone demoing flux wants first, epic or not.

## Acceptance

- [ ] **Failing-first**: a test asserting the parsed `Commands::Tui` carries the prompt, failing at
      the merge base because the field does not exist.
- [ ] `prompt: Vec<String>` matching `Commands::Run`'s spelling exactly — so `flux tui fix the
      failing test` works unquoted. ⚠ Do not invent a different shape (a `--prompt` flag, a single
      `String`) for the one verb that was missing it; the point is that the CLI stops having an
      exception.
- [ ] **With no prompt, behaviour is byte-identical to today.** This is the compatibility half and it
      is the half a test must pin — the splash path, the empty-input path, and the REPL behaviour are
      untouched.
- [ ] The prompt is submitted through the same path a typed first message takes, so approval, steering
      and history behave identically. ⚠ It must **not** bypass the approval envelope — a task supplied
      by argv is not more trusted than a task that was typed, and `--yes` remains the only way to
      auto-approve.
- [ ] `flux tui --help` describes it, and the doc comment on the variant says what an absent prompt
      does.
- [ ] Full gate green.

## Notes

- The existing doc comment on `Commands::Tui` ("Launch the ratatui chat TUI (requires a real
  terminal)…") needs updating, not replacing — the `--yes` sentence stays true.
- Check whether `AgentFlags` already carries anything prompt-shaped before adding a field; the flatten
  hides its contents at this call site.
- Related: the epic's renderer stories ([C-422](C-422-the-render-projection.md),
  [C-423](C-423-flux-cast.md)) do not depend on this landing, and this does not depend on them.

## Progress

- Filed 2026-08-01 with the session-screencast epic.
