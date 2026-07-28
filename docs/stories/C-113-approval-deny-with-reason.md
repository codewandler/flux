---
id: C-113
title: Approval deny-with-reason — tell the model why
pillar: Core
status: ready
priority: P3
design: tui-polish
epic: tui-polish
areas: [flux-tui]
note: extends C-103's approval_key contract — lands after it
---

# Approval deny-with-reason — tell the model why

## Goal
C-103's `approval_key` contract stops at Allow / AllowAlways / Deny: a bare deny gives the loop
nothing to adapt to and invites near-identical re-proposals. Add a fourth choice — `d` prompts
for a one-line reason that is fed back to the model with the denial.

## Acceptance
- [ ] `approval_key` gains DenyWithReason on `d`/`D`; the exhaustive unit tests are extended and
      stray keys still map to Ignore.
- [ ] `d` switches the sheet to a one-line reason input; Enter resolves the approval as denied
      carrying the reason; Esc returns to the sheet with the approval still pending (the reply
      oneshot unresolved) — TestBackend test for both paths.
- [ ] The reason reaches the model-visible denial feedback for the turn, as an ADDITION to the
      canonical op-anchored denial shape, never a mutation of it (the executor's denial text is
      pinned and classified structurally — see L-21/L-32).
- [ ] Deny-on-drop / deny-on-finish FIFO queue semantics unchanged; plain `n`/Esc keeps the
      reasonless fast path.

## Progress
-

## Notes
- Depends on C-103 (structured `ApprovalView` + `approval_key`).
- Seams: approval queue + `show_next_approval` `crates/flux-tui/src/controller.rs:47-64`, modal
  key branch `lib.rs:1846`, sheet render `rendering.rs:214`.
- The TUI's approver adapter reply channel likely needs to carry `Option<String>`; scope any
  flux-runtime seam change minimally and keep the default Deny path byte-identical.
