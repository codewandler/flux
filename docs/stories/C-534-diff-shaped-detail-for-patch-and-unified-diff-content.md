---
id: C-534
title: "Diff-shaped detail: patch from input edits, unified-diff content classifier"
pillar: Core
status: done
epic: tool-output-rendering
design: docs/designs/tool-output-rendering.md
areas: [flux-tui]
note: "git_diff and patch output renders flat muted while the diff renderer sits 1,500 lines away"
---

# Diff-shaped detail: patch from input edits, unified-diff content classifier

## Goal

Anything diff-shaped renders as a diff. `patch` gets a real hunk view synthesized from its input
`edits` array (exact and available pre-result, like `edit`), and unified-diff *content* — `git_diff`
output, `patch`/`edit`/`write` views, a `bash git diff` — is classified and colored instead of
rendering as flat muted text.

## Acceptance

- [x] `toolview::format_diff` gains a `patch` arm built from the `edits` input array; failing-first
      unit test in the toolview tests module.
- [x] `toolview::format_detail` classifies content that parses as a unified diff into
      `DetailKind::{Meta,Hunk,Add,Del}` rows; failing-first unit test on `git_diff`-shaped content,
      plus one TestBackend frame test asserting a `git_diff` card renders classified rows.
- [x] Non-diff content is never misclassified (a plain line starting with `-` in prose context does
      not flip the card into diff colors — the classifier requires diff structure, not a prefix).
- [x] The C-195 no-redaction stance is restated in the extended function docs, unchanged.
- [x] `cargo test -p flux-tui` green.

## Progress

- 2026-08-05 — filed from the tool-output review; implementation started same session.
- 2026-08-05 — closed on integrated local `main`: patch inputs produce honest input-anchored hunks,
  structured unified-diff content is classified without prefix-only false positives, the rendered
  TestBackend card preserves hunk/add/delete styling, and the complete `flux-tui` suite passes.

## Notes

- Evidence: `format_diff` handles only `edit`/`write` (`crates/flux-tui/src/toolview.rs:203-260`);
  `git_diff` returns a raw unified diff as content (`crates/flux-tools/src/lib.rs:2404-2415`);
  `patch` returns status + unified diff view via `edit_result` (`flux-tools/src/lib.rs:925`,
  `:2111-2115`), which live is the card's content until C-532 lands.
- Design: [tool-output-rendering](../designs/tool-output-rendering.md) F-4.
