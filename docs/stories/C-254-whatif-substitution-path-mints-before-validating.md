---
id: C-254
title: "`WhatIf::run`'s pure-substitution path mints before its own refusals, so \"no trace\" is still not a property of minting"
pillar: Core
status: ready
priority: 12
epic: typed-session-log
design: docs/designs/typed-session-log.md
areas: [flux-sdk, flux-flow]
note: "the third and last site of C-211's invariant — found by C-247's review, deliberately not widened into its diff"
---

# `WhatIf::run`'s pure-substitution path mints before its own refusals

## Goal
C-211 hoisted validation above the mint at both **fork** sites; C-247 did the same for `WhatIf::run`'s
**re-plan** path and its two bails. The pure-substitution (`!replan`) path was outside both stories'
enumerated Acceptance and still mints first, so C-211's invariant — **a failed operation leaves no
trace** — remains a property of *particular paths* rather than of session minting, which is how it
was framed in C-247's Goal.

Three refusals still fire after the child exists:

1. `build_frozen`'s `substitute_at({node}, _) targets a node with no recorded dispatch`
   (`crates/flux-sdk/src/whatif.rs:293-299`, called at `:465`) — after the mint at
   `crates/flux-sdk/src/whatif.rs:462`.
2. `rerun_pinned`'s `session {src} has no executed plan to rerun in turn {t}`
   (`crates/flux-flow/src/whatif.rs:377-382`).
3. The same `build_frozen` refusal on the **re-plan** path, which sits after C-247's mint
   (`whatif.rs:532` then `:548`) — C-247 hoisted the two refusals its Acceptance named, and this
   third one was not among them.

All three are pre-existing at `a0ad8219` and none is a regression from C-247.

## Acceptance
- [ ] **Failing-first test**: a substitution refused because the target node has no recorded dispatch
      leaves **no** child session behind — assert the session count is unchanged across the refusal,
      and `panic!` on the `Ok` arm. It fails today.
- [ ] A second failing-first case for `rerun_pinned`'s "no executed plan to rerun" refusal.
- [ ] A third for the `build_frozen` refusal on the re-plan path, which C-247 left after its mint.
- [ ] Every refusal above is hoisted above the mint, so no path in `WhatIf::run` creates before it has
      finished validating.
- [ ] The tests observe the *absence of a trace* (session count unchanged), not merely `is_err()` — an
      `is_err()`-only assertion passes with the child still minted, and is the specific way this class
      of fix gets faked.
- [ ] Standard gate green in both workspaces.

## Progress
- 2026-07-30 — filed from C-247's independent review, which confirmed C-247's own Acceptance was fully
  met and flagged this as the remaining scope rather than as rework. Recorded instead of widened, which
  is the same discipline that produced C-247 out of C-211 — and the reason all three sites are now
  written down rather than rediscovered a third time.

## Notes
- Read C-211's and C-247's diffs first; this is the same move applied to the last three sites.
  C-247's shape is the model: validate, mint, then one checked rewrite.
- Cheaper to hold as an invariant than to re-derive per path. Note the orphan is *self-cleaning* today
  (`prune_empty_excluding` gates on `last_seq <= 0`), so this is invariant hygiene rather than a leak —
  but C-247's review showed the turn-path orphan escaped `prune_empty` because it carried a full
  conversation, so "self-cleaning" does not generalise and should not be assumed for these three.
- `flux-sdk` is published; a refused operation simply stops creating a session, so the behaviour change
  is patch-compatible. It rides `version.workspace = true`, so `scripts/check-crate-versions.sh` owes
  nothing — state the surface impact anyway.
