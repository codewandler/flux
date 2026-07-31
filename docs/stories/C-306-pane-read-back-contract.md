---
id: C-306
title: "The pane read-back contract — can an agent ask what is on screen, and should it?"
pillar: Core
status: ready
priority: 17
epic: agent-authored-surface
design: docs/designs/agent-authored-surface.md
areas: [flux-runtime, flux-tools, flux-tui]
note: "C-223 could not ship pane.list: C-220 made SurfaceSink send-only ON PURPOSE and forbids tool-held pane state, so a listing would report panes the surface has already dropped. This is a contract decision, not an implementation detail"
---

# The pane read-back contract — can an agent ask what is on screen, and should it?

## Goal

Decide, and then implement or explicitly refuse, whether an agent can read back the state of the
surface it has been writing to — and settle `pane.list` either way.

## Why this exists

C-223 shipped `pane.open` / `pane.update` / `pane.close` and could not ship `pane.list`. That was the
right call rather than a shortfall: `SurfaceSink` is **send-only by design** (C-220), and C-223's own
note forbids the tool from holding pane state. A listing assembled from the model's own call history
would report panes the surface has already dropped — the surface expires panes at turn boundaries,
on `/resume`, and at the `MAX_PANES` cap — so it would be confidently wrong rather than merely
incomplete.

Closing it properly needs a query method on `SurfaceSink` plus an L2 listing type: a change to
C-220's contract. That is a design decision about whether the agent-authored surface is a
**write-only projection** or a **readable model**, and it should be answered once rather than
implied by an implementation.

## Acceptance

- [ ] **State the position in the design doc**, with reasoning, before any code: is the surface
      write-only, or readable? "Write-only" is a legitimate answer and closes this story without a
      new op — but it must be written down, because two live comments currently assume the opposite
      (see Notes).
- [ ] If readable: a query method on `SurfaceSink` and an L2 listing type, with `pane.list` built on
      it and **no pane state held in the tool**. A failing-first test that a listing reflects a
      host-side expiry the model never saw.
- [ ] If write-only: `pane.list` is formally withdrawn, and the two stale comments below are
      corrected so nothing claims a wiring that will never exist.
- [ ] Either way, answer the design's own open question — *"should `pane.list` report host panes?"* —
      which is the same read-back question wearing a different hat. Host-owned panes (the fleet pane
      from C-224) are the case that makes it concrete: an agent that can list them can also be
      confused by them.
- [ ] Full gate green.

## Notes

- **Two comments in the tree currently promise this wiring and will read as stale until this is
  settled:** `flux-tui`'s `ChatState::open_panes` / `panes::PaneListing` (C-224) carry a doc comment
  saying C-223 wires `pane.list` to them. Nothing can, for the reason above.
- Cheap cleanup to fold in if the contract is reopened anyway: the reserved-namespace rule now lives
  in **two** crates — `flux-tui`'s `panes.rs` and `flux-tools`' `surface.rs` (`HOST_ID_PREFIX`) —
  because a shared constant would need an L2 home. If an L2 listing type lands, that constant has a
  home too.
- Related: [C-223](C-223-pane-ops.md) shipped the write side;
  [C-305](C-305-run-tui-installs-the-surface-sink.md) makes it reachable and does **not** depend on
  this decision.
