---
id: A-135
title: "A later remote A2A fleet test needs an injectable, guarded transport"
pillar: Agent
status: backlog
epic: fleet-coordinator
design: docs/designs/fleet-coordinator.md
areas: [flux-a2a, flux-orchestrate]
note: "Decision 0010's native local V1 no longer depends on a stub A2A worker; retain this as remote-transport testability work"
---

# A later remote A2A fleet test needs an injectable, guarded transport

## Goal

When remote workers return to the fleet schedule, provide a deterministic A2A test seam without
weakening endpoint guards or confusing a socket-free coordinator test with wire coverage.

## Acceptance

- [ ] Decide between guarded injectable transport, op-boundary fixture and loopback wire fixture and
      state exactly which production behavior each test proves.
- [ ] A remote dispatch/status/cancel journey runs deterministically under the selected seam while
      existing real-wire coverage remains.
- [ ] Caller-supplied endpoints still pass through scoped URL guards; test injection cannot bypass
      authorization, egress policy or redaction.
- [ ] The story is not a dependency of A-117's native local offline journey and is promoted only with
      the remote fleet extension.

## Progress

- 2026-08-05 — former A-117 blocker removed by Decision 0010. The V1 journey uses native local
  sub-agents and requires no A2A socket or fake remote task server.

## Notes

- The archived pre-Decision-0010 fleet coordinator design preserves the original transport audit.
