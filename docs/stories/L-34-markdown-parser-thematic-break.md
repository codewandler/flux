---
id: L-34
title: "Terminate a list on a spaced thematic break instead of nesting an empty list"
pillar: Language
status: done
priority:
epic: review-hardening
design: docs/designs/review-hardening.md
note: "parse_list's next-item check consults only list_marker and never is_thematic_break, so `- - -` after a `-` item is consumed as another list item (an empty nested list) instead of terminating the list with a ThematicBreak — stray empty bullets in the CLI/TUI renderers and wrong re-emitted markdown"
---

# Terminate a list on a spaced thematic break instead of nesting an empty list

## Goal
Fix a CommonMark divergence in `flux-markdown`'s list parser. `parse_list`'s next-item check consults only
`list_marker` and never `is_thematic_break` (`crates/flux-markdown/src/parser.rs:335-336`), while
`parse_blocks` tests `is_thematic_break` before `list_marker` at block start (`:54-61`). So a spaced
thematic break like `- - -` following a `-` list item is consumed as another list item (an empty nested
list) instead of terminating the list with a `ThematicBreak`.

## Acceptance
- [x] Failing-first test (verified repro): `"- a\n- - -\n"` parses as a list followed by a `ThematicBreak`
      (the CommonMark result, matching what `parse_blocks` produces for the same line at block start). Today
      it parses as one list whose second item is a nested empty-list tower.
- [x] Fix: have `parse_list`'s next-item check reject a line that `is_thematic_break`, mirroring the
      block-start precedence.
- [x] Existing list-parsing tests pass unchanged.

## Progress
- 2026-07-03 filed — 0.2.11 diff review; grounded 🟡 correctness, confirmed with a runtime repro. Renders
  stray empty bullets in the CLI/TUI renderers and re-emits wrong markdown from the writer.
- 2026-07-03 fixed: `parse_list`'s next-item check (`crates/flux-markdown/src/parser.rs`) now breaks out
  of the list when the next line `is_thematic_break`, checked before the `list_marker` match — mirroring
  `parse_blocks`' block-start precedence. Added `parser::tests::spaced_thematic_break_terminates_list`
  (confirmed failing before the fix, reproducing the nested empty-list tower for `"- a\n- - -\n"`; passes
  after, yielding a 1-item list followed by `Block::ThematicBreak`). Full gate green: `cargo test -p
  flux-markdown` (40 tests incl. parity-oracle and round-trip suites), `cargo clippy -p flux-markdown
  --all-targets -- -D warnings`, `cargo fmt -p flux-markdown --check` — all clean, no parity-oracle
  conflicts.

## Notes
- Evidence: `crates/flux-markdown/src/parser.rs:335-336` (next-item check) vs `:54-61` (block-start precedence).
- Residual of [L-02](L-02-flux-markdown-engine.md). Pairs with [L-33](L-33-markdown-writer-fence-length.md).
  Design: [review-hardening](../designs/review-hardening.md).
