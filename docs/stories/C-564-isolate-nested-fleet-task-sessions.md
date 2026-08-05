---
id: C-564
title: "Nested Fleet tasks cannot steal a worker's continuation session"
pillar: Core
status: ready
priority: 5
epic: fleet-loop
design: docs/designs/fleet-agent-payload-budgets.md
areas: [flux-cli, flux-events, flux-flow]
depends_on: [C-560]
note: "dogfood defect — nested task turns changed the store's latest-session target, moving one worker from s_1 through s_4"
---

# Nested Fleet tasks cannot steal a worker's continuation session

## Goal

Keep one admitted worker bound to its own durable conversation when it invokes nested `task` work or
other child sessions in the same store.

## Acceptance

- [ ] Failing first, a worker with session `s_1` runs a nested task in its store; the next
      `--continue` currently selects the child as the latest session and records a new unrelated
      runtime session. The fixed worker resumes `s_1` by explicit identity.
- [ ] Fleet never uses mutable store “latest session” as the worker continuation key once an exact
      session id is recorded. Nested children have explicit parent/correlation and cannot replace it.
- [ ] Restart, compaction, rework and acknowledged messages preserve the same worker session until an
      explicit new-session transition is recorded.
- [ ] Missing/corrupt exact sessions fail with actionable bounded diagnostics; there is no silent
      fallback to whichever child happened to write last.
- [ ] Hermetic tests cover two workers sharing no session state, nested task success/failure and a
      continued turn after restart. Existing ordinary `--continue` behavior remains compatible.

## Notes

- Observed during C-560 wave-46: task operations advanced the recorded runtime session from `s_1`
  through `s_4` without an intentional worker-session transition.
