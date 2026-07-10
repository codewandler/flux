---
id: A-65
title: Make the TUI a dense daily driver
pillar: Agent
status: done
design: docs/designs/tui-makeover.md
note: "Borderless dense chat, durable session replay, core REPL controls, and a visible FIFO follow-up queue."
---

# Make the TUI a dense daily driver

## Goal
Turn `flux tui` into a powerful but minimal daily-driver interface: a borderless transcript, a
background-separated composer, honest session continuity, visible queued follow-ups, and the core
interactive controls already available in the REPL.

## Acceptance
- [x] `composer_is_background_only_without_border_or_padding` pins the dense composer contract.
- [x] Queued prompts are visible, editable, reorderable FIFO entries and are never overwritten.
- [x] New/resumed sessions project their full durable activity without re-executing anything.
- [x] Core REPL commands work in the TUI: plan/run, model, shell, tools/evidence, sessions, compact.
- [x] Terminal setup always unwinds; idle mode performs no periodic redraw; long transcripts render
      only the visible viewport.
- [x] Historical tool inputs are capped and redacted before persistence; old event records decode.
- [x] Structured input redaction covers JSON-escaped secrets; cassette-free traces still reconstruct
      reduced leaf-op cards while loop machinery stays hidden.
- [x] Queue edits retain their FIFO slot; pruning never removes the active session; `/model mock`
      remains offline; durable projection never replaces the engine's active model.
- [x] The full workspace gate and a real-PTY mock-provider smoke pass.

## Progress
- 2026-07-10: user-approved implementation started in the dedicated `feat/tui-makeover` worktree.
- 2026-07-10: implemented and verified. The PTY smoke covered bracketed multiline paste, approval,
  FIFO queue drain after cancellation, real session creation, durable resume, and terminal-mode
  restoration. Workspace build/test/clippy/fmt and `flux-codegate` are green.
- 2026-07-10: review hardening closed all six findings with failing-first regressions: escaped-secret
  persistence, active-session pruning, mock-model routing, queue edit ordering, cassette-free trace
  reconstruction, and startup model identity.

## Notes
- Full-screen alternate-screen mode remains the only mode in this story.
- Raw thinking, transient animation, true mid-turn steering, image attachments, autopilot commands,
  and the A-47 time-machine cockpit stay out of scope.
