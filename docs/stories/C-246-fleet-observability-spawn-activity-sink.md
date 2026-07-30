---
id: C-246
title: "Fleet observability — `SpawnActivitySink` shipped in A-79 and nothing installs it, so a running fleet is invisible"
pillar: Core
status: in-progress
priority: 5
epic: fleet-loop
design: docs/designs/fleet-loop.md
areas: [flux-flow, flux-tui]
note: "F10 — the sink exists and is installed for local children only (engine.rs:527); a fleet of workers produces no visible activity at all"
---

# Fleet observability — `SpawnActivitySink` shipped in A-79 and nothing installs it, so a running fleet is invisible

## Goal
A fleet you cannot watch is a fleet you cannot trust unattended. `SpawnActivitySink` was shipped by
A-79 and *is* installed in production — but only for local children
(`crates/flux-flow/src/engine.rs:527`). A fleet of workers started through `fleet.start` produces no
per-worker activity on any surface, so the operator sees a long silence and cannot tell a working
wave from a hung one.

Install the sink for fleet workers and surface per-worker status: the C-224 fleet pane in the TUI,
and/or a `fleet.monitor` journey (A-128). Sized down as needed — visible and honest beats complete.

## Acceptance
- [ ] **Failing-first test**: starting a fleet produces live, **redacted**, per-role activity on the
      surface. Assert there is no such activity at the merge base.
- [ ] Redaction is real, not incidental — a worker's secrets and credentials never reach the surface.
      Pin it with a corpus test, following the existing redaction seams rather than a new one.
- [ ] A hung worker is distinguishable from a working one on the surface (last-activity or status),
      which is the whole operational point.
- [ ] Standard gate green in both workspaces.

## Notes
- Depends on **F6 (C-243)** for workers to observe. Filed `ready` because the install seam and the
  redaction work can be scoped and tested against local children first.
- Related, already-filed work to reuse rather than duplicate: C-224 (the sub-agent/fleet pane) and
  A-128 (`fleet.monitor`).
- Do not add a second surface path. The existing sink seam is hardened; a parallel one would have to
  re-earn redaction and budget-bounding.
