---
id: C-110
title: Help overlay (F1 / /help)
pillar: Core
status: ready
priority: P2
design: tui-polish
epic: tui-polish
areas: [flux-tui]
note:
---

# Help overlay (F1 / /help)

## Goal
`/help` prints a notice into the transcript. Replace it with a proper centered overlay listing
keybindings and slash commands.

## Acceptance
- [ ] F1 and `/help` open a centered panel (session-picker style); Esc/F1/q close; other keys
      swallowed. Precedence: after the approval modal, before the session picker.
- [ ] Slash-command list iterated from the `COMMANDS` table (cannot drift); key list covers the
      epic's new bindings (Ctrl-T, Ctrl-R, Ctrl-F, approval keys).
- [ ] TestBackend test: open → `/model`, `/resume`, `Ctrl-J` visible; closed → absent.
- [ ] `HELP_TEXT` notice retired.

## Progress
-

## Notes
- Seams: `HELP_TEXT` `lib.rs:236`, `COMMANDS` `lib.rs:161`, `centered()` `rendering.rs:6`,
  session-picker overlay `rendering.rs:169`.
- Lands LAST in the epic so its content is complete.
