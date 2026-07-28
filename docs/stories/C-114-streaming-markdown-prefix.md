---
id: C-114
title: Markdown for the sealed prefix while streaming
pillar: Core
status: done
priority: P3
design: tui-polish
epic: tui-polish
areas: [flux-tui]
note: land after C-104 (flux_markdown hardcoded span colors are its known risk)
---

# Markdown for the sealed prefix while streaming

## Goal
Streaming assistant text renders fully plain (with a cursor) because "half-parsed Markdown
flickers"; styling pops in only when the message seals. Render the COMPLETED block prefix through
the markdown engine while streaming and keep only the trailing unterminated block plain — long
replies read styled as they arrive, without flicker.

## Acceptance
- [x] While streaming, text up to the last completed block boundary (a blank line outside an open
      code fence) renders via `markdown::render`; the tail stays plain + cursor.
- [x] Anti-flicker invariant pinned: across two successive stream states, the lines rendered for
      the previously-completed prefix are byte-identical (unit test on the boundary-split helper
      at two append states).
- [x] An open code fence stays plain in its entirety until the closing fence arrives (no
      misrender mid-fence) — test.
- [x] The sealed render path is unchanged: the final sealed output is byte-identical to today's
      (test compares before/after).

## Progress
- Done 2026-07-28: `split_sealed_prefix` (blank-line boundaries outside fences; successors that could restyle earlier blocks — indented/list/blockquote — held back; fence opener is a safe successor), streaming branch renders the sealed prefix via markdown::render cached under (width, prefix_len) — the cache IS the anti-flicker guarantee — tail plain + cursor; sealed path byte-identical. Tests: split_sealed_prefix_is_conservative_and_monotonic, streaming_renders_sealed_prefix_styled_and_stable.

## Notes
- Seams: streaming plain-text branch `crates/flux-tui/src/lib.rs:391`, sealed render
  `lib.rs:410`, pre-wrap wrapper `markdown.rs`.
- Boundary detection must be conservative: hold back anything a later line could reinterpret
  (setext underlines, list/paragraph continuations) — blank-line block boundaries only.
- Epic risk carried over: `flux_markdown` hardcodes span colors (C-104's known risk) — land after
  C-104 or accept dark-theme-only styling initially.
