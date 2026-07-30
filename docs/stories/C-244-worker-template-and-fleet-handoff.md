---
id: C-244
title: "The implement-worker template + `fleet.handoff` — a worker can only return text today, so reviewing the diff as evidence has no channel"
pillar: Core
status: backlog
epic: fleet-loop
design: docs/designs/fleet-loop.md
areas: [flux-flow, flux-server, flux-runtime, flux-a2a]
note: "F7 — where Task.artifacts stops being decorative: SpawnOutcome has no artifact field and flux-server never populates artifacts, so the review half of the loop is unimplementable without this"
---

# The implement-worker template + `fleet.handoff` — a worker can only return text today, so "review the diff as evidence" has no channel

## Goal
The loop's review half rests on a worker returning *evidence*: the branch it committed on, the
failing-first test it wrote, and the before/after of that test. Today a worker can return **only
text**. `SpawnOutcome` has no artifact field (`crates/flux-runtime/src/lib.rs:104-112`) and
`flux-server` never populates `Task.artifacts` — the type exists (`crates/flux-a2a/src/types.rs:248`)
and nothing sets it.

Ship two halves: a served `worker.flux` that, given an item and its worktree, implements, runs the
gate, commits on `impl/<id>`, and returns a **structured handoff as `Task.artifacts`**
(`{branch, test, before, after, summary}`); and `fleet.handoff` on the coordinator, which consumes
that artifact into the item's `evidence`.

## Acceptance
- [ ] **Failing-first test**: a worker returns its branch plus its failing-first evidence, and
      `fleet.handoff` writes them onto the board item's `evidence`. Impossible today — assert the
      artifact channel is absent at the merge base.
- [ ] `SpawnOutcome` carries structured artifacts, and `flux-server` populates `Task.artifacts` from
      them — spec-faithful A2A, not a side channel.
- [ ] The handoff is a typed shape with an `output_schema`, not prose the coordinator re-parses:
      `{branch, test, before, after, summary}`.
- [ ] `worker.flux` is served, and its gate run is real — a worker that reports green while the gate
      is red is caught by a test (the report is a claim; the gate output is the evidence).
- [ ] `fleet.handoff` refuses cleanly when the artifact is missing or malformed, rather than writing
      partial evidence.
- [ ] Standard gate green in both workspaces.

## Notes
- Depends on **F2 (C-240)** for `board.record_evidence` — that is the write this consumes — and on
  **F6 (C-243)** for a worker to hand off from.
- **Scope boundary, from the design's correction:** a returned branch *name* is useless from a remote
  worker, because its branch lives on another filesystem with no fetch path. This story serves the
  **local**-worker loop, where the worker shares the coordinator's `.git`. Artifact return over A2A
  for remote code workers is `agent-fleet-runtime`.
- `flux-a2a` and `flux-server` are on the published protocol line — a type change here obliges a
  version decision. Report the surface change; run `scripts/check-crate-versions.sh`.
