---
id: A-106
title: "Irreversibility disclosure — surface 'cannot be undone' at approval and to policy"
pillar: Agent
status: backlog
epic: transactional-turns
design: docs/designs/transactional-turns.md
note: "the risk signal that IS available at approval time because declaration is static; rides the C-182 op list + C-154 risk tint rather than adding a new sheet"
---

# Irreversibility disclosure — surface "cannot be undone" at approval and to policy

## Goal
Close the loop the epic opens: because `Compensation` is declared statically (A-103), the approval
gate can say *before* you approve that part of this batch can never be taken back — the story's
"no compensator declared becomes a policy-visible risk signal". This is the one piece of the epic
that does not depend on execution-time capture.

## Acceptance
- [ ] The plan-approval sheet gains one line when any op in the batch declares
      `Compensation::None`: `⚠ N operations cannot be undone — <op names>`. It rides the existing
      C-182 operation list and C-154 risk tint; no new sheet, no new modal.
- [ ] The distinction survives MONO/`NO_COLOR` (glyph + text, not colour alone) — consistent with
      the C-154 posture.
- [ ] The per-op approval path discloses it too, not just whole-plan approvals — this is exactly the
      dead-plumbing class C-154 found (an `IntentSet` received and discarded); pin it with a test on
      the single-op path.
- [ ] Policy can gate on irreversibility: an op declaring `Compensation::None` in a protected scope
      can be made `require_approval` (or denied) through the existing policy vocabulary — no new
      policy language.
- [ ] **Failing-first test**: a batch containing `send_email` currently discloses nothing about
      reversibility; after this story it discloses, and a test asserts the disclosure reaches the
      approver.
- [ ] `flux undo`'s report (A-105) and this disclosure use the same `why` strings — asserted, so the
      two surfaces cannot drift.

## Progress
- Not started.

## Notes
- Design: [transactional-turns.md](../designs/transactional-turns.md).
- Blocked by A-103 only (not A-104/A-105) — it can land before undo exists, and arguably should:
  telling users what is irreversible has standalone value even with no undo verb yet.
- `ApprovalRequest` gained `mutating` in C-154 and is already a breaking-change surface; adding an
  irreversibility field there rides the same MINOR.
