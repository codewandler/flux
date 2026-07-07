---
id: A-48
title: A2A stateful mode — one session per contextId, and a conversation-carrying A2aTurn seam
pillar: Agent
status: done
design:
epic:
note: "SHIPPED 2026-07-07 (same day as the live downstream report): flux-server reuses one session per contextId (memory with no client change) + additive A2aTurn::run_in_context seam so downstream mounts can key their own continuity"
---

# A2A stateful mode — one session per contextId, and a conversation-carrying A2aTurn seam

## Goal
Multi-turn agents must work over the A2A text channel. Today both flux A2A surfaces are
stateless per turn, so any slot-filling/multi-turn preset served over A2A text re-reads an empty
conversation every message and loops (single-turn FAQ-style agents are unaffected; voice works
because its WS session is long-lived). Wire-level symptom: repeating a request with an explicit
`contextId`/`taskId` still mints a fresh task.

## Why (evidence, verified 2026-07-07)
- `crates/flux-a2a/src/server.rs` (`send`, ~:133): *"Echo the conversation id for
  forward-compatibility with a future stateful mode (one session per contextId); today each turn
  is independent."* — and `Task::new(new_id(), context_id, …)` mints a new task id per call.
- `crates/flux-a2a/src/server.rs:26` — `A2aTurn::run(&self, input: &str)` /
  `run_rich(&self, input: &str)`: **no conversation identity crosses the seam**, so a downstream
  implementor structurally cannot key continuity even if it keeps its own session store.
- `crates/flux-server/src/a2a.rs:13-15`: *"Each task creates a fresh session (stateless A2A mode);
  the `contextId` from the request is echoed (and recorded as the session's correlation id) so a
  future stateful mode (one session per `contextId`) needs no client change."* — the C-18
  correlation tagging means the lookup key is ALREADY persisted on the streams registry.

## Acceptance
- [ ] `flux-server` mount: `message/send`/`message/stream` with a `contextId` that matches an
      existing live a2a session (correlation-id lookup, C-18 tags) REUSES that session — the
      engine's conversation projection then gives multi-turn memory for free; a new/absent
      `contextId` mints as today. TTL (C-18) keeps bounding lifetime; the C-29 queued-retention
      guard still holds. Failing-first integration test (the C-41 suite is the harness): two
      `message/send` calls with the same `contextId` → same session, and the second turn's answer
      demonstrates memory of the first (mock provider with scripted responses); different
      `contextId` → isolated sessions.
- [ ] `flux_a2a::server` helper: the turn seam gains conversation identity — additive, e.g.
      `run_in_context(&self, ctx: &A2aTurnContext, input: &str)` (carrying `context_id`, task id)
      with a default impl delegating to today's `run`/`run_rich` so existing implementors compile
      unchanged; `dispatch`/`send` pass the extracted `context_id` through. Failing-first test: a
      stateful test runner keyed on `context_id` accumulates turns across two `dispatch` calls.
- [ ] The two "today each turn is independent / stateless A2A mode" comments are updated to
      describe the shipped stateful mode (and the docs touched: `docs/a2a.md` continuity section).
- [ ] No client change required (the comments' promise): `contextId` is standard A2A; clients
      that never repeat a `contextId` keep exactly today's per-turn isolation.

## Progress
- 2026-07-07 DONE, all four acceptance boxes:
  - `flux-server`: `create_a2a_session` → resolve-or-mint via the new
    `EventStore::find_correlated(correlation_id, agent_id)` (newest live match wins, so a
    TTL-pruned conversation's `contextId` cleanly starts — and then continues — a fresh session).
    Sweep-before-lookup preserves C-18 semantics; C-29's mint-inside-the-gate discipline
    unchanged (the lookup happens at the same point minting did). Task id = session id stays
    stable across a continued conversation (documented).
  - `flux_a2a::server`: additive `A2aTurnContext { context_id }` +
    `A2aTurn::run_in_context(ctx, input)` with a delegating default — every existing implementor
    compiles and behaves unchanged (the pre-existing StubRunner/FailRunner/BlockRunner tests
    pass untouched); `dispatch`/`send` route through it.
  - Failing-first tests: `same_context_id_continues_the_session_with_memory` (memory-probe
    provider answers `seen:<user-msg-count>` — second call answers `seen:2` with the SAME task
    id; would answer `seen:1` with a fresh task id on the pre-fix code),
    `different_or_absent_context_ids_stay_isolated`,
    `dispatch_passes_context_so_a_stateful_runner_accumulates` (flux-a2a),
    `find_correlated_returns_newest_matching_tagged_stream` (flux-events).
  - Both "stateless mode" comments rewritten to describe the shipped stateful mode; docs/a2a.md
    continuity note + security-notes section updated (incl. the D-64 per-principal caveat).
  - The `flux a2a` chat client needed NO change — it already mints one `contextId` per chat
    session and sends it on every message (verified: `new_id()` once, threaded through
    `Message::user_text`).

## Notes
- Reported by the downstream consumer running a slot-filling preset over `flux a2a` text chat;
  the consumer ALSO has a stateless wrapper on its side to fix once the seam carries the id —
  that half is theirs; flux's half is this story.
- Related backlog: D-63 (multi-agent A2A mount, resolver-keyed) — session-per-contextId is a
  natural prerequisite/sibling; D-64 (request auth seam) would key per-principal isolation on top.
- Security note: session reuse must key on `(contextId)` only within the same bearer-auth realm
  the server already enforces; when D-64 lands, continuity must not leak across principals.
