---
id: C-115
title: Hunk-view diffs — line numbers + intraline highlight in edit/write cards and the approval sheet
pillar: Core
status: ready
priority: P3
design: tui-polish
epic: tui-polish
areas: [flux-tui]
note: card half is independent; the approval-sheet half needs C-103
---

# Hunk-view diffs — line numbers + intraline highlight in edit/write cards and the approval sheet

## Goal
Expanded `edit`/`write` cards show a flat classified diff (`DetailKind` add/del/meta lines).
Upgrade it to a real hunk view — `@@` headers, gutter line numbers, word-level intraline
highlight on changed spans — and reuse it as a content preview inside C-103's approval sheet,
which today shows tool + subjects but no diff where the approval decision actually needs eyes.

## Acceptance
- [ ] Expanded edit/write cards render hunks: `@@` headers with real old/new line numbers, a
      numbered gutter, and ± markers — extends the existing expanded-card TestBackend pinning
      test.
- [ ] Changed spans within a modified del/add line pair get word-level intraline emphasis; the
      hunk-numbering and intraline-split helpers are pure and unit-tested in `toolview` without
      the async loop.
- [ ] `toolview` keeps its color-free formatting contract: it returns structured kinds/spans and
      the TUI maps them to theme roles.
- [ ] The C-103 approval sheet embeds the same diff preview for pending `edit`/`write` calls,
      windowed alongside the subject list — TestBackend test with a multi-hunk edit.

## Progress
-

## Notes
- Seams: `DetailKind` `crates/flux-tui/src/toolview.rs:152`, the detail formatter
  `toolview.rs:168`, card color mapping `lib.rs:1094-1097`, sheet render `rendering.rs:214`.
- Line numbers need the edit's old/new text: decide in-story whether to compute the diff in
  `toolview` from the call args (`old_string`/`new_string`) or parse the unified diff the tool
  result already carries.
