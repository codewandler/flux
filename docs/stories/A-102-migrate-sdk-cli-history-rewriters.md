---
id: A-102
title: "Migrate the SDK/CLI history rewriters (fork, whatif, export) onto rewrite()"
pillar: Agent
status: done
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
- [x] `flux-sdk/session.rs:402` (fork) and `flux-sdk/whatif.rs:496` use `rewrite`, appending once.
- [x] `flux-cli/session.rs:314` (fork) uses `rewrite`.
- [x] `flux-cli/export_cmd.rs`, `flux-server` and `flux-events` test fixtures use the typed API;
      nothing in the workspace references the deleted helpers.
- [x] A fork of a session whose history ends mid-tool-pair is rejected with a `ShapeError` naming
      the invariant, rather than silently producing a child session that 400s on its first turn —
      **failing-first test** (today the fork copies the broken shape through unexamined).
- [x] Fork remains a pure read of the source stream: a test asserts the source's event count is
      unchanged.
- [x] Full gate green in both workspaces, including the `plugins/` workspace fmt check.

## Progress
- 2026-07-29 — **done.** The three rewriters (`flux-sdk` fork + `whatif` re-plan, `flux-cli` fork)
  now build a `ValidHistory` from the source conversation and install it with one
  `SessionLog::rewrite`. `EventStore::record_message`/`record_compaction` are **deleted** — the
  deletion A-101 deferred here — and every fixture in `flux-events`, `flux-cli`, `flux-server` and
  `flux-tui` moved to the typed seam.
- Failing-first: `fork_refuses_a_parent_history_that_ends_mid_tool_pair`
  (`crates/flux-sdk/src/lib.rs`) — before the change `Session::fork` **succeeded** on a parent whose
  log ends on an unanswered `tool_use` and minted the child with the broken shape
  (*"a fork of a mid-tool-pair history must be refused, not copied through — got s_2"*). Its sibling
  `fork_seeds_the_child_conversation_with_one_append` pinned the N→1 append change (*"one Compacted
  event installs the whole history: left: 0, right: 1"*).
- **What the deletion does not buy** (recorded in the design doc): `EventStore::append` +
  `NewEvent::message` remain public and equivalent. The short conversation-shaped name is gone, not
  the capability. Restricting `NewEvent`'s conversation constructors is a separate story — the
  store's own event-log and contention tests write off-shape logs on purpose.
- Where a fixture genuinely needs a shape the typed seam refuses (the `append` transactionality and
  C-124/C-126 contention tests; the TUI projection over a log that opens on an assistant message),
  it now calls `store.append(.., NewEvent::message(..))` explicitly, with a comment saying why.

- 2026-07-29 — **integrated.** Merged to `main` as `a88edf56` (implementor commit `1c5515a2`); full
  gate re-run green on the integration branch — workspace build, 144 test suites, clippy
  `-D warnings`, `cargo fmt --check` in both the root and `plugins/` workspaces, and `flux-codegate`.
  Closes the **typed session log** epic (A-93). **The next release cut is a MINOR** — the removal is
  a breaking change to the published `codewandler-flux-events`.

## Notes
- Design: [typed-session-log.md](../designs/typed-session-log.md).
- Blocked by A-100; must land in the same release as A-101 (which deletes the API these sites use).
- Worth checking while here whether `whatif` variants should share one `ValidHistory` rather than
  re-validating per variant — a perf note, not a correctness one.
