---
id: C-589
title: "Agents and operators can inspect the durable truth of a session (epic)"
pillar: Core
status: ready
priority: 2
epic: session-truth
design: docs/designs/session-truth-and-self-inspection.md
areas: [flux-events, flux-agent, flux-flow, flux-capabilities, flux-cli]
note: "s_2013 executed a real task-child uninstall, then later turns falsely denied it because chat context omitted execution provenance"
---

# Agents and operators can inspect the durable truth of a session

## Goal

Make Flux's existing durable session record directly queryable by id from both the CLI and agent
operations, then keep enough verified execution provenance across turns that an agent cannot erase a
real delegated action merely because its chat context does not contain tool events.

## Acceptance

- [ ] C-590 exposes one bounded, redacted session list/detail service through exact-id CLI selection,
      stable JSON and `session.list`/`session.inspect` operations without a second source of truth.
- [ ] C-591 records and reconstructs a host-derived last-turn execution receipt across ordinary
      continuation, compaction and resume, and teaches the direct-parent versus delegated-child
      capability boundary.
- [ ] A hermetic regression with the `s_2013` shape delegates a state-changing command to a child,
      verifies the result, then asks the parent how it happened; the answer names the `task` child
      and verified action and never retracts it as fabricated.
- [ ] Exact inspection includes child lineage, accepted action/status, effect class, event sequence,
      usage and explicit omission metadata while excluding raw reasoning, private instructions and
      unredacted secrets.
- [ ] Public session, operations, delegation and troubleshooting docs show how a human and an agent
      inspect `s_<id>` and explain that a child can have a different narrowed operation surface.
- [ ] Query and receipt paths remain read-only, bounded and backend-conformant; full tests, clippy,
      formatting, codegate and embedded-doc freshness pass.

## Progress

- 2026-08-05 — filed from a full durable audit of `s_2013`: 9 turns, 32 model calls, three correlated
  child sessions and a verified uninstall were present in events, while seven later assistant answers
  progressively replaced those facts with a false no-execution narrative.

## Notes

- C-164 finds sessions by conversation/file/time but cannot select an exact id, emit JSON or inspect
  actions and children. C-490 narrates aggregate facts; it is not the deterministic evidence API.
- C-212 covers opt-in history from foreign harnesses. This epic is the smaller, always-local Flux
  execution-record surface and must not silently widen into cross-harness transcript retrieval.
