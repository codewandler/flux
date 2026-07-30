---
id: A-111
title: "Fleet coordinator — flux orchestrating flux across repos (epic)"
pillar: Agent
status: ready
priority: 29
epic: fleet-coordinator
design: docs/designs/fleet-coordinator.md
note: "EPIC — the coordinator is a .flux Program on flux-app, not a new binary: a write-capable WorkBoard port (Jira/markdown/GitLab swappable), outbound A2A dispatch to remote workers, and per-delivery bus isolation as the blocker"
---

# Fleet coordinator — flux orchestrating flux across repos (epic)

## Goal
Make flux able to supervise a fleet of remote flux agents across repos: intake work from Jira or a
webhook, hold it on a board whose implementation is swappable, dispatch it to remote workers over
A2A, reconcile their status on a schedule, and report back — all as an ordinary `.flux` Program on
the existing `flux-app` host, with every board mutation an ordinary policy-gated op.

The design's central finding is that this needs **no new app**. `flux-app` already runs Programs
with agents/channels/datasources/triggers/journeys, a bus and a delivery supervisor; `flux-channels`
already supplies cron/webhook/slack/a2a adapters; `plugins/jira` already has issue CRUD and
transitions. What is missing is a write-capable state port, an outbound A2A op, and concurrent
delivery.

## Acceptance
- [x] A design doc (`docs/designs/fleet-coordinator.md`) covering the `WorkBoard` port and why it is
      a purpose-built sibling of `LiveDatasource` rather than an extension of it, the four backends,
      outbound A2A dispatch, run state, per-delivery bus isolation, and the multi-root question —
      each claim about the current tree pinned at `file:line`.
- [ ] The epic is broken into implementation stories on the board (A-112…A-118); each behavioral
      change ships with a failing-first test.
- [ ] Headline proof: `flux run coordinator.flux --serve` drives an end-to-end intake → dispatch →
      sweep → done cycle against `MemoryBoard` and a stub A2A worker, **offline**, in CI.

## Progress
- 2026-07-29 — **design done**: [fleet-coordinator.md](../designs/fleet-coordinator.md). Grounded in
  the current tree; three findings changed the plan:
  - The crate remembered as "flux-coord" is `flux-orchestrate` (L3), and it is **in-process only**
    (`LocalSpawner`, the `task` op, dependency waves). It is not the coordinator substrate —
    `flux-app` (L6) is, and it is further along than expected.
  - The "abstract the state source" instinct matches a port flux already built:
    `LiveDatasource` (`crates/flux-capabilities/src/datasource/live.rs:60`) declares a schema plus
    its external authority, is validated once, and the host *generates* uniform ops with stable
    permission subjects, a tool group and an ambient signal. It is **read-only**, which is exactly
    the gap.
  - The A2A **server** task surface is already complete (A-53…A-57, in
    `crates/flux-server/src/a2a.rs`). The missing half is the **client**: `A2aClient` has no
    `cancel`, and its only callers are `crates/flux-cli/src/a2a_cmd.rs:131,218` — no journey or op
    can dispatch to a remote agent.

## Notes
- Order: **A-112 first** (per-delivery bus isolation — a coordinator whose sweep blocks intake is
  single-threaded by construction), then A-113 → A-114 / A-115 / A-116 in parallel → A-117 → A-118.
- Release mechanics: A-113 touches `flux-datasource` (protocol line — explicit version decision,
  caught only by `scripts/check-crate-versions.sh`); A-112 is likely breaking for `flux-app`
  embedders ⇒ pre-1.0 that is a **MINOR**.
