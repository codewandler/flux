---
id: L-33
title: "Size a tilde code fence by its tilde runs, not backtick runs"
pillar: Language
status: done
priority: 4
epic: review-hardening
design: docs/designs/review-hardening.md
note: "the markdown writer computes fence length from the longest backtick run even when it emits a TILDE fence (info string contains a backtick), so a body with a `~~~` run closes the fence early — parse(to_markdown(parse(src))) != parse(src), silently corrupting any document round-tripped through the writer"
---

# Size a tilde code fence by its tilde runs, not backtick runs

## Goal
Restore `flux-markdown`'s stated round-trip contract `parse(to_markdown(parse(src))) == parse(src)` for
code blocks. `to_markdown` picks a tilde fence when the info string contains a backtick, but computes the
fence length from the longest *backtick* run in the literal (`crates/flux-markdown/src/writer.rs:35-41`),
never considering the body's tilde runs. So a body containing a 3+ tilde run closes the emitted fence
early and the block is split/lost on re-parse.

## Acceptance
- [x] Failing-first test (verified repro): input `"~~~~a`b\n~~~\n~~~~\n"` (one code block, info `` a`b ``,
      literal `~~~`) must round-trip to a single equivalent code block. Today it re-emits a 3-tilde fence
      whose first body line `~~~` terminates it, and re-parsing yields two empty code blocks.
- [x] Fix: when emitting a tilde fence, size it from the longest tilde run in the literal (mirror the
      backtick-fence logic); when emitting a backtick fence, keep the backtick-run logic.
- [x] Existing writer/round-trip tests pass unchanged.

## Progress
- 2026-07-03 filed — 0.2.11 diff review; grounded 🔴 silent data corruption, confirmed with a runtime repro.
- 2026-07-03 fixed: `to_markdown`'s `Block::CodeBlock` arm (`crates/flux-markdown/src/writer.rs`) now
  picks the fence character (`` ` `` vs `~`) first, then measures the longest run of *that* character
  in the literal (previously always measured backtick runs, even for a tilde fence). Added
  `writer::tests::tilde_fence_sized_from_tilde_run_not_backtick_run` (confirmed failing before the fix,
  reproducing the two-empty-code-block split on `"~~~~a`b\n~~~\n~~~~\n"`; passes after). Full gate green:
  `cargo test -p flux-markdown` (40 tests incl. parity-oracle and round-trip suites), `cargo clippy -p
  flux-markdown --all-targets -- -D warnings`, `cargo fmt -p flux-markdown --check` — all clean, no
  parity-oracle conflicts.

## Notes
- Evidence: `crates/flux-markdown/src/writer.rs:35-41` (`longest` from `literal.split(|c| c != '`')` yet
  line 40 emits a tilde fence when `info.contains('`')`).
- Residual of [L-02](L-02-flux-markdown-engine.md). Pairs with [L-34](L-34-markdown-parser-thematic-break.md).
  Design: [review-hardening](../designs/review-hardening.md).
