---
id: C-548
title: "A session-scoped board survives continue, replay and fork"
pillar: Core
status: ready
priority: 43
epic: first-class-board
design: docs/designs/native-board-fleet-cli.md
areas: [flux-session, flux-capabilities, flux-sdk]
note: "Decision 0010 session scope — board mutations are session events, not an in-memory side table"
---

# A session-scoped board survives continue, replay and fork

## Goal

Implement the `session` board backend as durable session state so a scratch or execution board lives
for exactly the agent session that owns it without becoming repository files.

## Acceptance

- [ ] Failing-first integration test creates and mutates a session board, closes the client, resumes
      the session and reads the identical item/revision history; the backend is absent at the base.
- [ ] Board mutations append typed session events and reconstruct through the normal session store.
      Replay consumes recorded events without performing a second live mutation.
- [ ] A fork inherits the recorded prefix and then diverges independently; parent and child
      revisions and comments cannot leak into each other after the fork point.
- [ ] Session retention removes the board only when its owning session is retired; no standalone
      cache or cleanup policy silently shortens its life.
- [ ] Concurrent revisions use C-547's optimistic conflict contract and cancellation cannot leave a
      partially visible transition.
- [ ] The backend passes the complete contract suite for its selected general/planning/execution
      profile and produces the same permission subjects as other backends.
- [ ] SDK and CLI tests cover binding to the current session and the explicit `--session ID|last`
      selector outside a live run.
- [ ] Targeted session, capability and SDK tests pass; the final board wave owns the full gate.

## Notes

- Depends on A-134's registry/profile contract and C-547's revision envelope.
