---
id: A-08
title: Sub-agent audit ON by default — child runs land correlated in the shared event store
pillar: Agent
status: ready
priority: 7
note: the D-05 with_audit seam exists but the CLI never wires it — every child spawn falls back to a throwaway in-memory EventStore, so sub-agent tool activity vanishes; only usage bubbles up
---

# Sub-agent audit ON by default

## Goal
Make "you can always explain what the agent did" (vision.md:98) true for sub-agents on the primary
path. Verified 2026-07-02: `LocalSpawner` supports `.with_audit(Arc<EventStore>)` (D-05, done) but
the default `flux run` spawner is built without it (`crates/flux-cli/src/main.rs:1371-1374`), so
`audit == None` and every spawn falls back to a throwaway `EventStore::in_memory()`
(`crates/flux-orchestrate/src/lib.rs:301-303`) — the child's run trace and conversation are
discarded, and the parent's `/evidence` never shows child activity.

## Acceptance
- [ ] **Failing-first:** `sub_agent_run_lands_in_shared_audit_store_with_correlation`
      (flux-orchestrate) — spawner `.with_audit(shared)`; after a child run, a second stream exists
      with `EventContext.agent_id == "subagent:<role>"`, `correlation_id == parent session`, and
      (with C-14) `observation` events for the child's tool calls (fails today: throwaway store).
- [ ] CLI: the session `EventStore` is created before `SubAgents` and wired via
      `.with_audit(events.clone())` — default ON, no config knob (clean cutover). Same for the
      flux-app spawner path. `audit: None → in_memory` remains as the documented *ephemeral* mode
      for storeless hosts (SDK consumers, tests) — a real mode, not a compat bridge.
- [ ] Correlation via the D-02 envelope: child sessions created with
      `create_session_with_context` (`agent_id: "subagent:<role>"`, `correlation_id: parent
      session id`). Parent session id reaches the spawner via `ToolContext::set_session/session_id`
      (set per turn beside `set_cancel`) + a single-method `Spawner::spawn(SpawnRequest { role,
      task, cap_scope, parent_session }, cancel)` (clean trait cutover).
- [ ] Parent `/evidence` gains ONE compact `subagent.trace` observation (child session id +
      tool-call/error counts) next to the existing `subagent.usage` — the full child trail is
      already durably flushed under its own correlated stream by C-14 (no wholesale copy, no
      double-persist).
- [ ] Full gate green; CHANGELOG entry.

## Progress
- Filed 2026-07-02 from the harness claims review (P7 of the round).

## Notes
- Strictly after C-14 (needs `EventKind::Observation` + the child engine's flush).
- Pre-existing, unchanged: parent Ctrl-C can leave an unterminated child turn under with_audit
  (orchestrate lib.rs:317-320; documented in the D-05 design's Known limitations).
