---
id: C-195
title: "Decide and enforce redaction on the approval sheet's diff preview"
pillar: Core
status: done
priority:
epic: security-assurance
design: docs/designs/security-assurance.md
note: "CLOSED AS WON'T DO — the approval sheet does not redact; redaction is a boundary control (persistence/model/machine sinks), and scrubbing the sheet would hide a pending credential write from the one person able to deny it. Decision in docs/designs/security-assurance.md, recorded at the seam on toolview::format_diff"
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
- [x] The design question is answered in writing first: should the approval sheet — a local,
      human-eyes decision surface whose entire job is to show the operator exactly what is about to
      be written — redact credentials, or is redaction correct only on log/model-facing paths? The
      decision and its rationale land in `docs/designs/security-assurance.md` (or its own doc) before
      any code. → **§ "The approval sheet does not redact (C-195)"** in
      `docs/designs/security-assurance.md`, written before any code change.
- [ ] If the decision is to redact: a `Redactor` is threaded into the approval render path (the
      `flux-secret` dependency edge added to `flux-tui`), and a failing-first test seeds a credential
      onto an added hunk line and asserts it does not reach the rendered sheet. → **not applicable**:
      the decision is not to redact. No `flux-secret` edge was added; `crates/flux-tui/Cargo.toml` is
      unchanged.
- [x] If the decision is **not** to redact: the reason is recorded at the seam in code so the next
      reviewer does not re-file this, and this story closes as "won't do" with that pointer. →
      doc comment on `toolview::format_diff` (`crates/flux-tui/src/toolview.rs`) + a pointer at the
      approval-sheet call site (`crates/flux-tui/src/rendering.rs`), plus the decision-pinning test
      `diff_does_not_redact_credentials_by_decision`.
- [x] Standard gate green in both workspaces.

## Progress
- 2026-07-29 — split out of C-185 during impl-coord integration. C-185's redactor fix (items 1-3, 5)
  shipped and gated green; item 4 was descoped here because it is a dependency + design change, not
  the boundary-set fix C-185 scoped.
- 2026-07-29 — **decided: won't do.** The approval sheet does not redact, and `flux-tui` gains no
  `flux-secret` edge. The argument, in full, is in `docs/designs/security-assurance.md`; the four
  load-bearing points:
  1. Redaction would erase the highest-value catch the sheet exists to enable ("this write puts a
     live credential in a file"). It changes the render, not the write — so the operator reads
     `+api_key=[redacted]`, approves, and the real value still lands on disk. It converts a
     catchable leak into an invisible one, and suppresses the signal that a secret already reached
     the model's context.
  2. An approval surface must be WYSIWYG; `Redactor` is a lossy heuristic (fixed `SECRET_PREFIXES`
     list, ≥6-char substring registration) that both under- and over-matches. Cosmetic on a log
     line, a correctness defect on the operator's last look at pending bytes.
  3. It would buy no confidentiality property. `format_diff` has three callers — the sheet and the
     transcript tool card — so the same bytes render seconds later regardless; `CliSink::tool_call`
     and `plan_prompt` are raw too (a `curl -H "Authorization: Bearer …"` shows in full at the CLI
     approval prompt today). Every local human-eyes path in flux is unredacted, uniformly.
  4. Redaction is a *boundary* control and already covers these bytes where it matters:
     `Executor::dispatch` redacts results not inputs (`flux-runtime/src/lib.rs:3637-3645`), and the
     input gap is closed at the serialization boundary instead
     (`flux-cli/src/stream_json.rs:103-114` redacts the whole line). `whatif.rs` shows the split
     exactly: redact into the cassette (`:172`, `:228`, `:269`), pass the live sink through (`:195`).
  Doctrine as written already agrees: `flux-secret/src/lib.rs:173-174` ("before it is logged or
  shown to the model"), `AGENTS.md` ("logs or model-visible output"), `docs/architecture.md:164-166`
  (scoped to tool *output*), `SECURITY.md` (secret *exfiltration*).
- Reopen conditions are recorded in the design doc: (a) the TUI grows a persistence/sharing path —
  redact at the point of persistence, not at the render; (b) the broader C-132/C-185 "redact
  conversation text at write time" question lands, at which point the sheet follows that decision as
  one surface among many.

## Notes
- Seam: the C-115 hunk preview in `crates/flux-tui/src/rendering.rs:570-572`
  (`toolview::format_diff`), fed by `pending_approval_input`.
- Related: the broader "conversation text is never redacted at write time" question C-132/C-185
  flagged (every reader's own `Redactor` is the only control) is a separate, larger decision — do
  not fold it in here.
- Source: [C-185](C-185-redactor-line-marker-boundary.md) acceptance item 4.
