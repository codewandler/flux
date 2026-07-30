---
id: C-246
title: "Fleet observability — `SpawnActivitySink` shipped in A-79 and nothing installs it, so a running fleet is invisible"
pillar: Core
status: done
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
- [x] **Failing-first test**: starting a fleet produces live, **redacted**, per-role activity on the
      surface. Assert there is no such activity at the merge base.
- [x] Redaction is real, not incidental — a worker's secrets and credentials never reach the surface.
      Pin it with a corpus test, following the existing redaction seams rather than a new one.
- [x] A hung worker is distinguishable from a working one on the surface (last-activity or status),
      which is the whole operational point.
- [x] Standard gate green in both workspaces.

## Progress
- 2026-07-30 — **done**, scoped to local children as this story's Notes sanction. Recovered as an
  orphan: the implementor was killed mid-task by a coordinating session crash, having left the work
  uncommitted but crate-test green. Branch preserved verbatim first, then reviewed independently, then
  integrated.
  **What the ticked items actually cover, so nobody re-reads them as more:** the surfaced workers are
  local children (`task` via `LocalSpawner`). Remote fleet workers emit nothing observable — a worker's
  tool activity is not visible over A2A (`crates/flux-orchestrate/src/fleet.rs:113`) — so item 1 is
  **not** met for `fleet.dispatch`, and full-fleet visibility still waits on **F6 (C-243)**. The Notes
  filed this story `ready` on exactly that basis: the install seam and the redaction work were scoped to
  local children first.
  The install is on the **production** path, which was the risk worth checking: a real `else if` arm in
  `CliSink::observation` (`crates/flux-cli/src/rendering.rs:835`), no `cfg(test)`, no feature gate,
  reachable through `flux flow run`, and the smoke test drives the real binary rather than installing the
  sink itself. A test-only install would have satisfied the Acceptance and left the fleet just as
  invisible.
  Failing-first was reconstructed analytically, since a killed implementor files no `BASE_PROOF`: the
  smoke test's only discriminator is the literal `⚇ fleet` substring, and `git grep -l "⚇" a0ad8219`
  returns nothing — the glyph occurs nowhere in the base tree, so no base binary can print it, and
  `crates/flux-tui/src/fleet.rs` did not exist there either.
  Redaction is structural rather than a second seam, per the Notes' instruction: `apply` never reads
  `ToolCall.input` or `Observation.observation`, pinned by an 8-shape secret corpus test with a
  non-vacuity assertion.
  Known limitation, recorded rather than fixed: the age-refresh reprint only fires when some worker
  emits, so if the *whole* wave hangs the line freezes at its last idle age. A single hung worker is not
  self-announcing; the multi-worker case item 3 is about does work.

## Notes
- Depends on **F6 (C-243)** for workers to observe. Filed `ready` because the install seam and the
  redaction work can be scoped and tested against local children first.
- Related, already-filed work to reuse rather than duplicate: C-224 (the sub-agent/fleet pane) and
  A-128 (`fleet.monitor`).
- Do not add a second surface path. The existing sink seam is hardened; a parallel one would have to
  re-earn redaction and budget-bounding.
