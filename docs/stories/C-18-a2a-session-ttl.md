---
id: C-18
title: A2A task sessions are never pruned — TTL-based cleanup
pillar: Core
status: ready
priority: 1
note: every `tasks/send` creates a session that lives forever in events.db — add TTL-scoped pruning for A2A-created sessions (the in-code TODO at flux-server a2a.rs)
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
- (not started — filed 2026-07-02 from the in-code TODO during the ready-queue curation.)

## Notes
- The events store is append-only by design — "pruning" must respect that philosophy: either a real
  `DELETE` restricted to whole expired streams (the TODO's sketch) or a tombstone the projections
  skip; pick in-implementation and document the choice in the design section of the commit/story.
