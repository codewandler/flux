---
id: C-94
title: "Approval distillation — the policy that learns from the audit trail (epic)"
pillar: Core
status: backlog
epic: approval-distillation
design:
note: "EPIC — mine the event store's approve/deny history into proposed durable policy grants; attacks approval fatigue without weakening default-deny"
---

# Approval distillation — the policy that learns from the audit trail (epic)

## Goal
Every approve/deny already lands in the event store, but nothing mines it. After the 30th time a
user approves `cargo test`, flux should propose the durable policy grant itself: "you've approved
this exact shape 30 times across 12 sessions — add a scoped rule?" Denials distill into deny rules
the same way. Nearest existing work is D-58 (risk approver) and
`docs/designs/typed-authority-requirements.md`, which are static; nothing closes the loop from
approval history back into policy. This attacks approval fatigue — the actual reason people flip
other harnesses to yolo mode — without weakening default-deny.

## Acceptance
- [ ] A design doc (`docs/designs/approval-distillation.md`) covering: the approval-shape
      fingerprint, the proposal threshold/heuristics, the user-consent flow (flux proposes, the
      human ratifies — never a silent grant), and how denials distill into deny rules.
- [ ] The epic is broken into implementation stories on the board; each behavioral change ships
      with a failing-first test.
- [ ] Headline proof: a repeated-approval history produces a proposed scoped grant, and accepting
      it removes the prompt for exactly that shape — with default-deny untouched everywhere else.

## Progress
- (not started — epic filed from the 2026-07-28 out-of-the-box ideas session)

## Notes
- Approvals/denials are already durable in events.db (C-14 evidence trail); this closes the loop
  back into `flux-policy`.
- Related but static: D-58 (RiskApprover), C-62 (typed authority contract).
- Ranked highest-leverage of the six ideas: compounds the existing envelope moat and fixes the top
  UX pain.
