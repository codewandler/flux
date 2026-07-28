---
id: C-103
title: Approval modal — explicit keys, real subjects, redesigned sheet
pillar: Core
status: ready
priority: P1
design: tui-polish
epic: tui-polish
areas: [flux-tui]
note:
---

# Approval modal — explicit keys, real subjects, redesigned sheet

## Goal
Today any stray key denies a pending approval, and subjects are rendered via `{:?}`. Make only
explicit keys act, render subjects as text, and redesign the sheet (accent border, windowed
subject list, colored key hints).

## Acceptance
- [ ] Pure helper `approval_key(code)` → Allow (y/Y), AllowAlways (a/A), Deny (n/N/Esc),
      Scroll (↑/↓), Ignore (everything else) — exhaustively unit-tested; on Ignore the modal stays
      and the reply oneshot is NOT resolved (test asserts the receiver is still pending after a
      stray key, then resolves Deny on `n`).
- [ ] `ChatState.modal: Option<String>` replaced by structured `ApprovalView { tool, subjects, scroll }`;
      `show_next_approval` passes subjects as `Vec<String>` — no Debug formatting (render test
      asserts a subject path appears verbatim, no `["…"]`).
- [ ] Redesigned sheet: accent-bordered block above the composer, bold tool name, subjects one per
      row (muted, truncated), windowed with `+N more` marker scrollable via ↑/↓, key hints styled
      (`[y]` ok / `[a]` warn / `[n/Esc]` err) — pinned by a TestBackend test with 10 subjects.
- [ ] Deny-on-drop / deny-on-finish FIFO queue semantics unchanged.

## Progress
-

## Notes
- Seams: modal key branch `lib.rs:1846`, `show_next_approval` + queue (event-loop locals)
  `controller.rs:47-64`, sheet render `rendering.rs:214`, `state.rs:13`.
- `modal` is a pub field → this is one of the epic's two MINOR breaks (batch with C-104).
