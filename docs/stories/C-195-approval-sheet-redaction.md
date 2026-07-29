---
id: C-195
title: "Decide and enforce redaction on the approval sheet's diff preview"
pillar: Core
status: backlog
priority:
epic: security-assurance
design: docs/designs/security-assurance.md
note: "SPLIT FROM C-185 item 4 — flux-tui performs NO redaction at all (no flux-secret dep, no Redactor in the crate); covering the approval sheet's hunk preview is a new dependency edge plus a design decision, not a boundary-set change"
---

# Decide and enforce redaction on the approval sheet's diff preview

## Goal
C-185 fixed the shared redactor so a diff/list marker can no longer hide a credential. Its fourth
acceptance item — "the approval sheet's diff preview is covered" — turned out to rest on a false
premise: `flux-tui` performs **no** redaction at all. It has no `flux-secret` dependency
(`crates/flux-tui/Cargo.toml`), no `Redactor` anywhere in `crates/flux-tui/src/`, and the sheet's
preview reads raw tool input via `pending_approval_input` → `toolview::format_diff`
(`crates/flux-tui/src/rendering.rs:570-572`), which never sees a redactor. Settle whether the
approval sheet should redact, and if so, enforce it.

## Acceptance
- [ ] The design question is answered in writing first: should the approval sheet — a local,
      human-eyes decision surface whose entire job is to show the operator exactly what is about to
      be written — redact credentials, or is redaction correct only on log/model-facing paths? The
      decision and its rationale land in `docs/designs/security-assurance.md` (or its own doc) before
      any code.
- [ ] If the decision is to redact: a `Redactor` is threaded into the approval render path (the
      `flux-secret` dependency edge added to `flux-tui`), and a failing-first test seeds a credential
      onto an added hunk line and asserts it does not reach the rendered sheet.
- [ ] If the decision is **not** to redact: the reason is recorded at the seam in code so the next
      reviewer does not re-file this, and this story closes as "won't do" with that pointer.
- [ ] Standard gate green in both workspaces.

## Progress
- 2026-07-29 — split out of C-185 during impl-coord integration. C-185's redactor fix (items 1-3, 5)
  shipped and gated green; item 4 was descoped here because it is a dependency + design change, not
  the boundary-set fix C-185 scoped.

## Notes
- Seam: the C-115 hunk preview in `crates/flux-tui/src/rendering.rs:570-572`
  (`toolview::format_diff`), fed by `pending_approval_input`.
- Related: the broader "conversation text is never redacted at write time" question C-132/C-185
  flagged (every reader's own `Redactor` is the only control) is a separate, larger decision — do
  not fold it in here.
- Source: [C-185](C-185-redactor-line-marker-boundary.md) acceptance item 4.
