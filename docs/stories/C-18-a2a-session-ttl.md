---
id: C-18
title: A2A task sessions are never pruned — TTL-based cleanup
pillar: Core
status: done
priority:
note: A2A sessions are now minted tagged (agent_id "a2a", contextId as correlation_id) and swept lazily at each mint — whole expired streams (age = last activity, TTL [server] a2a_session_ttl_secs default 3600, 0 = never) are DELETED via the new EventStore::prune_inactive; covers both the standalone server and the flux-channels a2a mount with no caller changes
---

# A2A task sessions are never pruned — TTL-based cleanup

## Goal
Sessions created for A2A tasks (`crates/flux-server/src/a2a.rs` — every `tasks/send`/SSE request calls
`create_session`) are never cleaned up, so a long-running server accretes one stream per task forever.
Add a TTL-based cleanup pass, as the in-code TODO (a2a.rs:185) already sketches.

## Why
"You can always explain what the agent did" needs the event log to stay navigable; unbounded
zombie-session growth also bloats `events.db` and every all-streams projection (`cost_summary_all`,
`efficiency_all`). A server surface should be able to run for weeks.

## Acceptance
- [ ] **Scoped pruning.** Only sessions *created by the A2A surface* are eligible — tag them at
      creation (the D-02 context envelope: `agent_id`/`correlation_id`) and prune by that tag +
      age. A CLI/TUI session must never be pruned. Failing-first test
      `a2a_ttl_prunes_only_expired_a2a_sessions`.
- [ ] **TTL knob.** Default 1h, configurable (`[server] a2a_session_ttl_secs` in flux-config,
      0 = never prune). Test covers the disable value.
- [ ] **Cleanup pass** runs on a background timer in `serve_on` (or lazily on request); pruning an
      active (recently-updated) session is impossible — age measured from last activity, not
      creation. Test `recently_active_a2a_session_survives_pruning`.
- [ ] Gate green: `cargo test --workspace`, clippy `-D warnings`, fmt, `cargo test -p flux-codegate`.

## Progress
- **Done (2026-07-02).** All acceptance boxes hold:
  - **Tagging:** every A2A session (`message/send` + `message/stream`) is minted via
    `create_a2a_session` — D-02 envelope `agent_id = "a2a"` (mirroring flux-orchestrate's
    `subagent:<role>` precedent), request `contextId` recorded as `correlation_id`.
  - **Sweep:** lazy per-request (at mint), not a background timer — covers every router mount
    (standalone server AND the flux-channels `a2a` channel) with zero caller changes; sweep
    failure logs and never blocks the task. TTL resolved once at router build from layered
    flux-config (`[server] a2a_session_ttl_secs`, default 3600, `0` = never; project-over-user
    merge per the `[limits]` precedent).
  - **Deletion choice (documented):** real DELETE of whole expired streams via the new
    `EventStore::prune_inactive(agent_id, cutoff_ms)` (one txn; extends `prune_empty`'s
    whole-stream reasoning) — append-only holds *within* a stream; retention is a stream-level
    decision. Age = `streams.updated_at` (last activity), never creation time. Projections stay
    consistent by construction (verified: post-prune `cost_summary_all`/`efficiency_all`).
  - **Tests (failing-first):** flux-events tag-scoping/cutoff/idempotence suite; flux-server
    `a2a_ttl_prunes_only_expired_a2a_sessions` (through the real `/a2a` handler; untagged CLI
    session survives), `recently_active_a2a_session_survives_pruning` (activity- not
    creation-based), `a2a_ttl_zero_disables_pruning`; flux-config default/merge test.
- **Rider:** repaired `projection::tests::cost_summary_rolls_up_session`, which C-20's gpt-5.5
  rate fix (4×) had broken on committed HEAD (expectation 11.25 → 35.0) — C-20's package gate
  didn't cover flux-events.
- **Residuals:** `/webhook` and `POST /sessions` still create untagged never-pruned sessions
  (out of scope — natural follow-up); TTL is read at router build (config change needs a server
  restart; no env/flag override was asked for); pruned sessions' spend leaves aggregate rollups —
  retain-forever deployments set `ttl = 0` (documented).
