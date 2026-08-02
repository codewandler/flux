---
id: C-490
title: Derive grounded insights from the durable session log
pillar: Core
status: done
design: docs/designs/session-derived-insights.md
note: "flux insights reports today's work; /insights reports the active session; code derives every fact and one tool-free model call narrates them"
---

# Derive grounded insights from the durable session log

## Goal

Turn Flux's durable session facts into an auditable daily or current-session report without asking a
model to rediscover counts, outcomes, usage, or operation history from prose.

## Acceptance

- [x] Failing-first tests prove that the projection scopes `flux insights` to the local calendar day
      and `/insights` to the complete active session, includes delegated detail without double-counting
      delegated usage, and remains complete across compaction.
- [x] The visible fact block is derived programmatically from stored events; its aggregate counts cover
      the whole scope while the model packet keeps newest detail within a disclosed 64 KiB UTF-8 cap.
- [x] Exactly one tool-free provider request narrates non-empty facts, optional slash-command direction
      changes focus only, and an empty day performs no provider construction or call.
- [x] The request and returned prose pass through credential-shape redaction; no tool-result body enters
      the fact packet.
- [x] `flux insights [-m <provider/model>]` and `/insights [direction]` work in the REPL and TUI; TUI work
      waits for idle state and stays cancellable.
- [x] Each attempted summary records one unscoped `CallUsage` without persisting report prose or erasing
      legacy turn-total accounting in event, CLI-usage, or TUI projections.
- [x] Public docs, CHANGELOG, WHATS-NEW, and its website mirror describe the new surfaces honestly.
- [x] The full workspace build/test/clippy/fmt/codegate gate is green.

## Progress

- 2026-08-02: request scoped and implementation plan agreed; story and design trail opened before code.
- 2026-08-02: shipped deterministic daily/session projection, one-call narration, CLI/REPL/TUI
  surfaces, durable unscoped accounting, public documentation, and a green full workspace gate.

## Notes

- The worktree already contains user-owned L-127/L-128 documentation-workbench changes, including edits
  to the CLI command enum, dispatcher, public docs, changelogs, and website mirror. Integrate without
  overwriting or reformatting those changes wholesale.
