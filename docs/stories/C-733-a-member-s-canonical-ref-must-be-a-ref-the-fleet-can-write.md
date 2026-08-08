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

`connectors` and `exchange` declare `canonical_ref = "origin/main"` — a remote-tracking ref that no
fleet operation can write. `apply` cannot reach it, `promote` refuses it by name, and C-721's check
reports both members' waves as *applied-without-delivery*.

Decision [0021](../../../flux-roadmap/decisions/0021-delivery-publication-and-release-are-three-events.md)
§2 says board validation must **refuse** the combination rather than accept it and silently
under-deliver. Today the misconfiguration is accepted at load and only discovered at the moment work
fails to land.

The config change alone is not safe — see Progress. The order is: reconcile each member's local
`main`, then repoint, then add the validation that stops it coming back.

## Acceptance

- [ ] Board validation refuses a `canonical_ref` that is a remote-tracking ref, at load, naming the
      member and what to declare instead. A refusal at configuration time is the whole point: the
      current failure surfaces only after a wave has been built and gated.
- [ ] The refusal message distinguishes "this ref does not exist" from "this ref exists and cannot be
      written", because they need different fixes.
- [ ] `connectors` and `exchange` are repointed at a local branch **after** their local `main` is
      reconciled with its remote. Reconciliation is a precondition of the repoint, not a follow-up —
      repointing first makes every dispatched worker branch from stale code.
- [ ] The unpushed `connectors` commit is preserved and its disposition recorded. It is not
      discarded to make the reconcile easy.
- [ ] The website's example fleet configuration carries the same mistake and is corrected, so the
      documented starting point does not reproduce the defect.
- [ ] `flux fleet doctor` reports zero `applied-without-delivery` findings afterwards, which is the
      observable proof the two historical waves were the symptom of this and not of something else.
- [ ] Regression test: a fixture config declaring `origin/main` is refused with the message above.
- [ ] Full gate green: `scripts/release-full-gate.sh`.

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
