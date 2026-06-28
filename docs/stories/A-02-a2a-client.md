---
id: A-02
title: A2A client — talk to a remote agent like a local one (flux a2a <URL>)
pillar: Agent
status: done
priority: 1
design:
---

# A2A client — `flux a2a <URL>`

## Goal
flux can be *exposed* over A2A (the `flux-server` agent card + endpoint) but cannot *consume* a
remote A2A agent. Add `flux a2a <URL>` so flux connects out to **any** spec-conformant A2A agent and
drives it from the CLI exactly like a local agent — an interactive REPL plus a one-shot/piped mode.
Serves the Agent pillar: flux becomes a first-class A2A **client**, not just a server.

A2A is an *agent* protocol, not a *model* protocol: the remote runs its own loop (model + tools), so
the client is thin — **one user turn = one remote A2A task** — and does NOT wrap the remote as a
`Provider` behind the local `FlowEngine`.

## Acceptance
- [x] New leaf crate `flux-a2a` (L1, no flux deps) with spec-conformant wire types + `A2aClient`
      (`fetch_agent_card`, `send`, `stream`, `get_task`); unit tests cover serde round-trips of a
      `message/send` params object and a `status-update` SSE frame, and URL/well-known normalization.
- [x] `flux-server` speaks the **current** A2A spec (`message/send` + `message/stream`, parts keyed
      by `kind`, `Task`/`TaskStatusUpdateEvent` results, both `/.well-known/agent.json` and
      `/.well-known/agent-card.json`) via the shared `flux-a2a` types — the old `tasks/send` /
      `tasks/sendSubscribe` draft methods are deleted (clean cutover). Updated in-file server tests
      prove the new request shape.
- [x] `flux a2a <URL>` opens an interactive REPL; `flux a2a <URL> <prompt…>` and piped stdin run a
      single turn and exit. Streamed deltas render live; Ctrl-C cancels a turn.
- [x] End-to-end: the client talks to our own `flux serve` (card discovery + streamed chat).
- [x] Gate green: `cargo test --workspace`, `clippy -D warnings`, `fmt`, `flux-codegate` lint (the
      new crate is classified in the `layer()` map).

## Progress
- Plan approved: `~/.claude/plans/zany-cooking-seal.md` (decisions locked: spec-conformant +
  general client, stateless MVP carrying `messageId`/`contextId`/`taskId`, server clean cutover).
- **Done.** `flux-a2a` crate (types + `A2aClient`, 7 unit tests); `flux-server` cut over to the
  current spec (5 server tests); `flux a2a` subcommand (REPL + one-shot + piped, live markdown
  render via `flux_markdown::render`, Ctrl-C cancel). Docs (`docs/a2a.md`) + CHANGELOG updated.
- **Verified end-to-end offline** against our own server with the `mock` provider: card discovery
  (`agent-card.json` + legacy alias), `message/send` (spec `Task`), `message/stream` SSE
  (`kind:"status-update"` deltas), and `flux a2a` one-shot + piped both rendered the reply. Full
  gate green (build / test / clippy `-D warnings` / fmt / `flux-codegate`).
- Optional follow-up: a live-model run (`-m openrouter-anthropic/anthropic/claude-sonnet-4.6`) for
  a multi-token streaming check; server-side statefulness per `contextId` (deferred, see Notes).

## Notes
- Stateless MVP (one turn = one task, matching today's stateless `flux serve`), but the client
  carries the spec identifiers so a stateful remote keeps memory and server-side statefulness can be
  added later **without client changes**.
- Reuses CLI primitives: `repl_history_path()`/a sibling, `run_interruptible()`,
  `markdown_terminal::LiveRenderer`, `style::*`. SSE via `eventsource_stream::Eventsource` (already
  used in `flux-providers/src/messages/mod.rs`).
- Files: `crates/flux-a2a/*` (new), `crates/flux-server/src/{a2a.rs,lib.rs}`,
  `crates/flux-cli/src/main.rs`, `crates/flux-codegate/src/lib.rs` (layer map), `docs/a2a.md`.
- Deferred: server-side statefulness per `contextId`; `-m a2a/<url>` provider mode; TUI surface;
  file/data parts + artifact streaming; `tasks/cancel` / push-notification methods.
