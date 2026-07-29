---
id: A-102
title: "Migrate the SDK/CLI history rewriters (fork, whatif, export) onto rewrite()"
pillar: Agent
status: ready
priority: 4
epic: typed-session-log
design: docs/designs/typed-session-log.md
note: "fork and whatif replay history message-by-message through the raw API today — rewrite() gives them the shape guarantee AND one append instead of N"
---

# Migrate the SDK/CLI history rewriters (fork, whatif, export) onto rewrite()

## Goal
The history-*rewriting* call sites outside `flux-flow` — session fork, `whatif` variants, and the
export/test helpers — replay messages one at a time through the unguarded `record_message`, so they
inherit no shape guarantee at all and pay N appends for one logical operation. Move them onto
`rewrite(ValidHistory)`.

## Acceptance
- [ ] `flux-sdk/session.rs:402` (fork) and `flux-sdk/whatif.rs:496` use `rewrite`, appending once.
- [ ] `flux-cli/session.rs:314` (fork) uses `rewrite`.
- [ ] `flux-cli/export_cmd.rs`, `flux-server` and `flux-events` test fixtures use the typed API;
      nothing in the workspace references the deleted helpers.
- [ ] A fork of a session whose history ends mid-tool-pair is rejected with a `ShapeError` naming
      the invariant, rather than silently producing a child session that 400s on its first turn —
      **failing-first test** (today the fork copies the broken shape through unexamined).
- [ ] Fork remains a pure read of the source stream: a test asserts the source's event count is
      unchanged.
- [ ] Full gate green in both workspaces, including the `plugins/` workspace fmt check.

## Progress
- Not started.

## Notes
- Design: [typed-session-log.md](../designs/typed-session-log.md).
- Blocked by A-100; must land in the same release as A-101 (which deletes the API these sites use).
- Worth checking while here whether `whatif` variants should share one `ValidHistory` rather than
  re-validating per variant — a perf note, not a correctness one.
