---
id: C-733
title: "A member's canonical_ref must be a ref the fleet can write"
pillar: "Core"
status: backlog
priority: 2
epic: delivery-is-verified
areas: [flux-orchestrate]
note: "connectors and exchange declare canonical_ref = origin/main, a remote-tracking ref no fleet operation can write. apply cannot reach it, promote refuses it by name, and C-721 reports both members' waves as applied-without-delivery. Decision 0021 section 2 says board validation must refuse the combination rather than accept it and silently under-deliver. Needs the config change plus the validation, and the website example config carries the same mistake"
---

# A member's canonical_ref must be a ref the fleet can write

## Goal


## Acceptance

- [ ] Define acceptance.

## Progress

- The config change alone is **not safe**, and this is the finding that matters. Both members do
  have a local `main`, so the ref exists — but measured 2026-08-08 they are badly stale against
  their remotes:
  - `connectors`: local `main` is **1 ahead, 61 behind** `origin/main`. The 1 ahead is an unpushed
    local commit that must not be discarded.
  - `exchange`: local `main` is **0 ahead, 169 behind** `origin/main`. Its checkout is also sitting
    on `wave-1-live-evidence`, not `main`, which is why `flux board transition X-139 …` reports
    `not-found`: the story file exists on main and not on that branch.
- So repointing `canonical_ref` at `main` would make every dispatched worker branch from
  169-commit-stale code and gate against it. The reconciliation of those local mains is a
  prerequisite, not a detail, and it needs the operator's judgement about the unpushed connectors
  commit.
- Order this story's work as: reconcile each member's local `main` with its remote → repoint
  `canonical_ref` → add the board validation that refuses a remote-tracking ref, so the
  misconfiguration cannot return.
- Until then `flux fleet promote` refuses both members by name, which is the correct behaviour and
  is why this is a configuration defect rather than a code one.
