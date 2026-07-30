---
id: C-247
title: "`WhatIf::run`'s re-plan path mints the child before validating, so a refused re-plan still leaves a trace"
pillar: Core
status: ready
priority: 6
epic: typed-session-log
design: docs/designs/typed-session-log.md
areas: [flux-sdk]
note: "the same defect C-211 fixed at both fork sites, still live in whatif.rs — and here there are TWO bails after the child exists, not one"
---

# `WhatIf::run`'s re-plan path mints the child before validating, so a refused re-plan still leaves a trace

## Goal
C-211 established the invariant **a failed operation leaves no trace** by hoisting
`ValidHistory::new` above `create_session_with_context` at both fork sites. `WhatIf::run`'s re-plan
path has the identical defect and was deliberately left out of C-211's scope, because that story's
Acceptance named the two *fork* sites and enumerated exactly them.

In `crates/flux-sdk/src/whatif.rs`, `dst` is minted at ~`:440` and only validated at ~`:497`. Worse
than the fork case, the same function has a **second** leave-a-trace bail — `session {src} has no
turn {target_turn} to re-plan` at ~`:504-508` — which also fires after the child session exists. So
a refused re-plan can leave an orphan by two different routes.

Hold the invariant everywhere it applies, so it stops being a property of the fork path and becomes
a property of session minting.

## Acceptance
- [ ] **Failing-first test**: a re-plan refused because the parent history is invalid leaves **no**
      child session behind — assert the session count is unchanged. It fails today.
- [ ] A second failing-first case for the other bail: a re-plan refused because the target turn does
      not exist also leaves no child session behind.
- [ ] Both checks are hoisted above the mint in `WhatIf::run`, mirroring C-211's shape at the fork
      sites — the validation, not the mint, comes first.
- [ ] The refusals stay clean recoverable errors naming the session and the reason, as C-211's do.
- [ ] Standard gate green in both workspaces.

## Progress
- 2026-07-30 — filed from the C-211 implementation. The implementor found it, correctly did not widen
  its own diff to cover it, and recorded it rather than dropping it. Hoisting the history check is
  mechanical; the turn-existence check is a second judgement call (it needs the turn resolved before
  anything is created), which is why this is its own story rather than a C-211 hunk.

## Notes
- Sibling of C-211 (`docs/stories/C-211-fork-validates-before-minting-child.md`) — read its diff
  first; this is the same move applied to a third and fourth site.
- Self-cleaning today for the same reason C-211's orphan was: nothing is appended to the orphan, so
  `last_seq == 0` and `prune_empty` collects it
  (`crates/flux-events/src/store/sqlite.rs:664-671`). The reason to fix it anyway is the same too —
  "a failed operation leaves no trace" is cheaper to hold as an invariant than to re-derive from a
  pruning rule on every read of the path.
- `flux-sdk` is published. A behaviour change to a public method is patch-compatible here (a refused
  re-plan simply stops creating a session), but state the surface impact explicitly and run
  `scripts/check-crate-versions.sh`.
