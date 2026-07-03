---
id: A-24
title: "Reserve marker/header in context byte budgets — stop len > cap overshoot"
pillar: Agent
status: done
epic: library-hardening
design: docs/designs/library-hardening.md
note: "render_knowledge_blocks appends the omission marker after the budget check (returns ~57B over budget); symbols_block_bounded counts only kept lines' byte length and omits its own header+marker — both overshoot their cap and neither pins len<=cap"
---

# Reserve marker/header in context byte budgets — stop len > cap overshoot

## Goal
Make the two context byte-budgets actually bound their output. `render_knowledge_blocks` truncates a
middle block to consume the *whole* remaining budget, then appends the omission marker **after** the check
(`crates/flux-core/src/context.rs:134`) → returns ~57 B + separator over `budget`. `symbols_block_bounded`
accumulates only kept symbol lines via `line.len()` (bytes) and never counts its fixed header or the trailing
"… N older omitted" marker (`crates/flux-flow/src/compile.rs:1173`) → overshoots `SYMBOLS_CHAR_CAP` by ~110 B,
and clips ~3× early on multibyte summaries because it measures bytes while claiming "10k chars."

## Acceptance
- [ ] Failing-first test `render_knowledge_blocks_stays_within_budget`: over-budget input with a truncated
      middle block returns a string with `out.len() <= budget` (today ~budget+57).
- [ ] Failing-first test `symbols_block_stays_within_cap`: a near-cap symbol set renders `block.len() <= cap`
      including header + marker (today overshoots).
- [ ] Fix: reserve the marker/header length up front (subtract before truncating) in both paths.
- [ ] Decide chars-vs-bytes for the symbols cap: either count `chars().count()` or rename the constant/comment
      to "bytes" so the A-07 "10k chars" claim is truthful.

## Progress
- 2026-07-03 DONE — `render_knowledge_blocks` reserves the omission-marker length before truncating (`out.len() <= budget`); `symbols_block_bounded` seeds the tally with the header + reserves the marker, and `SYMBOLS_CHAR_CAP`→`SYMBOLS_BYTE_CAP` with an honest doc. Tests: `render_knowledge_blocks_stays_within_budget`, `symbols_block_stays_within_cap`. Full gate green.

## Notes
- Evidence: `crates/flux-core/src/context.rs:121`,`:134`; `crates/flux-flow/src/compile.rs:1157`,`:1173`,`:1177`.
- Folds two audit findings (knowledge-block + symbols-block overshoot). Residual of
  [A-19](A-19-context-block-injection.md) / [A-07](A-07-bound-symbols-block.md).
  Design: [library-hardening](../designs/library-hardening.md).
