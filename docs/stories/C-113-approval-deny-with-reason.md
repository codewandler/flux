---
id: C-113
title: Approval deny-with-reason — tell the model why
pillar: Core
status: done
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
- [x] `approval_key` gains DenyWithReason on `d`/`D`; the exhaustive unit tests are extended and
      stray keys still map to Ignore.
- [x] `d` switches the sheet to a one-line reason input; Enter resolves the approval as denied
      carrying the reason; Esc returns to the sheet with the approval still pending (the reply
      oneshot unresolved) — TestBackend test for both paths.
- [x] The reason reaches the model-visible denial feedback for the turn, as an ADDITION to the
      canonical op-anchored denial shape, never a mutation of it (the executor's denial text is
      pinned and classified structurally — see L-21/L-32).
- [x] Deny-on-drop / deny-on-finish FIFO queue semantics unchanged; plain `n`/Esc keeps the
      reasonless fast path.

## Progress
- Done 2026-07-28: `ApprovalChoice::DenyWithReason(String)` (new variant — all existing Deny sites untouched; 4 exhaustive matches extended; reason APPENDED to the canonical `` `{op}` denied by user `` text + approval.denied evidence payload; reply channel unchanged — the reason rides inside the choice, simpler than the Option<String> the notes guessed). TUI: `d`/`D` → reason-input line on the sheet (Enter resolves, Esc back, empty reason falls back to plain Deny). Tests: deny_with_reason_appends_to_the_canonical_denial_text (runtime), deny_reason_input_renders_and_resolves_both_paths, approval_key exhaustive extended. Breaking pub enum variant → rides 0.28.0.

## Notes
- Depends on C-103 (structured `ApprovalView` + `approval_key`).
- Seams: approval queue + `show_next_approval` `crates/flux-tui/src/controller.rs:47-64`, modal
  key branch `lib.rs:1846`, sheet render `rendering.rs:214`.
- The TUI's approver adapter reply channel likely needs to carry `Option<String>`; scope any
  flux-runtime seam change minimally and keep the default Deny path byte-identical.
