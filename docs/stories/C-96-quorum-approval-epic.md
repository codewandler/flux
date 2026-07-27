---
id: C-96
title: "Quorum approval — the two-person rule for agents (epic)"
pillar: Core
status: backlog
epic: quorum-approval
design:
note: "EPIC — policy can require N distinct approvers for destructive ops in protected scopes on served/channel agents; the identity plumbing already exists"
---

# Quorum approval — the two-person rule for agents (epic)

## Goal
The approval seam is a single-human gate. For served/channel agents (D-04, D-69 multi-principal
work), let policy require N distinct approvers for destructive ops in protected scopes —
`git push` to main needs two humans. The identity plumbing (`TurnIdentity`, per-principal
isolation) already exists; no spec composes it into multi-party approval, and no competing harness
has it.

## Acceptance
- [ ] A design doc (`docs/designs/quorum-approval.md`) covering: the quorum policy vocabulary
      (N distinct approvers per protected scope), pending-approval state across principals,
      distinct-identity verification via the frozen `TurnIdentity`, and timeout/veto semantics.
- [ ] The epic is broken into implementation stories on the board; each behavioral change ships
      with a failing-first test.
- [ ] Headline proof: a destructive op in a protected scope on a served agent executes only after
      two distinct principals approve, and a single approver (or the same principal twice) is
      refused — pinned by a no-bypass test.

## Progress
- (not started — epic filed from the 2026-07-28 out-of-the-box ideas session)

## Notes
- Builds on D-69 (per-principal isolation), D-68 (request-auth seam), and the immutable
  `TurnIdentity` invariant; composes existing plumbing rather than adding a new identity path.
