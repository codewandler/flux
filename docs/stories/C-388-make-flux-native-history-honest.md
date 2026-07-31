---
id: C-388
title: Make flux-native history honest — compaction markers and a typed unsupported result
pillar: Agent
status: backlog
epic: agent-change-recovery-and-provenance
design: docs/designs/agent-change-recovery-and-provenance.md
note: "HarnessKind::Flux has no adapter and returns Ok(MessageStats::default()) — an empty result indistinguishable from 'nothing matched' — while the honest report.unsupported signal never reaches the model; EventKind::Compacted is persisted and never projected"
---

# Make flux-native history honest

## Goal

Let a retrospective see where context was rewritten, and stop an unread source from looking like an
empty one.

## Acceptance

- [ ] `search(harness: "flux")` against an assembly with no adapter returns a **typed unsupported**
      result, not an empty list — `report.unsupported`
      (`crates/flux-capabilities/src/datasource/harness_history.rs:323`) reaches the model.
- [ ] `EventKind::Compacted` (`crates/flux-events/src/kind.rs:50-55`) is projected as a marker
      record, so completeness after compaction or resume is observable rather than assumed — the
      exact property HAR-04 says cannot be proven today.
- [ ] Failing-first: a synthetic in-process log with N messages plus a `Compacted` event yields a
      compaction marker ordered between the pre- and post-compaction messages.
- [ ] C-302 (`ready`) owns the flux message adapter itself; this story is the honesty contract
      around it and states the dependency.

## Progress

- 2026-08-01 — filed from validation of HAR-04.

## Notes

- Same class as OUTCOME-01: a successful-looking empty result standing in for "I could not read this".
