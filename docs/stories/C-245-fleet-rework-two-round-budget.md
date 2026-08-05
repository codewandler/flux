---
id: C-245
title: "`fleet.rework` returns evidence to the same session twice, then parks"
pillar: Core
status: done
epic: fleet-loop
design: docs/designs/native-board-fleet-cli.md
areas: [flux-tools, flux-orchestrate, flux-runtime]
note: "host rule — delivered acknowledgement proves same-session steering; attempts cannot be laundered through board transitions"
---

# `fleet.rework` returns evidence to the same session twice, then parks

## Goal

Keep review feedback with the worker context that produced the change and make the two-round budget a
durable host invariant rather than prompt advice.

## Acceptance

- [x] Failing-first test proves two REWORK decisions are delivered to the same persistent session and
      a third returns PARK without dispatching. Evidence comes from session/event ids, not self-report.
- [x] Findings are structured path/line, command-output or invariant records with reviewer identity
      and reviewed commit; the worker chooses the fix.
- [x] Durable attempts are authoritative across restart, resume and board blocked/ready transitions.
      No transition, cancel or new CLI request can buy a fourth delivery.
- [x] PARK records unresolved findings, preserves the worktree/commit and transitions execution state
      without marking the planning story done.
- [x] `flux fleet message` uses the same acknowledged steering path and exposes accepted/delivered/
      completed wait levels while status remains readable during delivery.
- [x] Idempotent replay of a rework request never consumes another attempt. Cancellation has a
      terminal typed result.
- [x] Targeted runtime/orchestrate tests pass; A-117's integrated fleet wave owns the full gate.

## Notes

- Depends on C-244's typed handoff and delivered C-240/C-243.
