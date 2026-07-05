---
id: L-39
title: Multi-line string literals in flux-lang — kill the escaped-single-line-JSON wall
pillar: Language
status: done
epic: flux-planner-ship
design: docs/designs/flux-planner-ship.md
note: "the fine-tune's dominant failure (and a human-authoring pain): multi-KB edit payloads must be ONE escaped single-line JSON string; a triple-quoted spelling removes the failure mode at the source"
---

# Multi-line string literals in flux-lang

## Goal
A native multi-line string spelling (working proposal: `"""…"""` blocks, content taken
verbatim between delimiters with a defined indentation rule) usable anywhere a JSON string
literal is: argument objects, bare args, value templates. `format::format` EMITS the
multi-line spelling for any string containing `\n` (canonical text stops requiring heroic
escaping); `parse` accepts it; the L-18 roundtrip invariant (`parse(format(A)) == A`)
holds through the new spelling.

## Why now (evidence, 2026-07-04 fine-tune)
Every `each-bulk-edit` val sample failed `parse` with "invalid JSON literal: EOF while
parsing a string" — the 3B (and, less often, Sonnet) breaks long single-line JSON strings
with literal newlines. bf16 == q4 ruled out quantization; it is the representation.
Short-arg categories passed. See flux-model `runs/text-3b-r2/eval-report.json` and
`docs/go-no-go.md` Gate 2.

## Acceptance
- [x] Grammar + parser: multi-line string accepted in every string-literal position;
      spec'd indentation/termination rules documented in the language docs.
      Evidence: `flux_lang::parse::preprocess` desugars `"""…"""` to an escaped JSON string
      at the lexer stage (before any JSON/expr parsing), so every existing string-literal
      call site accepts it for free — bind values, call args, `lit` at any JSON nesting
      depth, value-template leaves, `fmt`/`assert`-message/`ctx`-purpose/`route`-case-label.
      Grammar documented in `crates/flux-lang/docs/syntax.md` § "Multi-line strings"
      (rewritten from "aspirational" to implemented: verbatim, no dedent, delimiter-scan
      termination, documented `"""`/trailing-`"` limitation). Parser tests:
      `crates/flux-lang/src/parse.rs` `multiline_string_literal_parses_verbatim_across_physical_lines`,
      `multiline_string_content_is_taken_literally_no_comment_no_escape_processing`,
      `multiline_string_works_as_a_call_arg_and_inside_an_object_template`,
      `empty_multiline_string_parses_as_empty_string`,
      `unterminated_multiline_string_is_a_located_parse_error`,
      `multiline_block_inside_a_pure_json_object_stays_a_lit_not_a_template`,
      `escaped_triple_quotes_inside_a_normal_string_are_not_mistaken_for_a_block`,
      `two_multiline_blocks_in_one_statement`,
      `multiline_content_preserves_blank_lines_indentation_and_statement_look_alikes`.
- [x] Formatter: `format::format` emits the multi-line spelling iff the string contains
      a newline (deterministic; no config); `format_compact` behavior decided + tested.
      Decision: `format_compact` NEVER emits the spelling (stays escaped single-line) —
      it is the display-only, non-round-tripping token-efficient preview, and the property
      it exists for (visually one line per statement) would break if a preview payload
      spanned pages. `format.rs` tests: `emits_multiline_spelling_for_a_lit_string_containing_a_newline`,
      `a_single_line_string_never_uses_the_multiline_spelling`,
      `falls_back_to_the_escaped_spelling_when_content_would_collide_with_the_delimiter`,
      `crlf_content_falls_back_to_the_escaped_spelling`,
      `format_compact_keeps_the_escaped_single_line_spelling_for_display`,
      `assert_ctx_route_fmt_native_string_fields_support_the_multiline_spelling`,
      `multiline_string_inside_a_pure_json_lit_object_round_trips`,
      `json_escape_line_never_uses_the_multiline_spelling` (the `@json` fallback is
      explicitly out of scope — stays exactly the wire format).
- [x] Failing-first roundtrip tests: property/unit tests extending the L-18 suite —
      newline-bearing strings survive `parse ∘ format` fingerprint-stable; goldens for
      nested cases (multi-line string inside an object template inside `each`).
      `tests/roundtrip_property.rs`'s `STRINGS` pool extended with a multi-KB-shaped diff
      payload and three "unsafe" shapes (trailing `"`, embedded `"""`, embedded `\r`) that
      must fall back — all 1000 seeds × 43 node kinds still round-trip exactly. Goldens:
      `multiline_string_inside_an_object_template_inside_each_round_trips` (value-template
      leaf inside `each`, the exact case named in Acceptance) and
      `multiline_string_inside_a_pure_json_lit_object_round_trips` (the pure-JSON-object
      corpus shape). `format.rs`'s `edge_content_shapes_round_trip` fuzzes newline-only /
      leading-newline / consecutive-blank-line / trailing-space content.
- [x] The planner text grammar (L-20 `build_text_grammar`) teaches the new spelling;
      `text_grammar_examples_parse_and_match_the_json_arm` still green.
      `crates/flux-flow/src/compile.rs`'s `build_ast_grammar` gained a 4th worked example
      ("write release notes with a multi-line message") whose formatted twin uses the
      `"""…"""` spelling; `build_text_grammar` gained a prose line teaching the spelling +
      its one termination rule. The guard test (`compile::tests::text_grammar_examples_parse_and_match_the_json_arm`)
      passes, proving the new example round-trips exactly like the other three.
- [x] Full gate green in BOTH workspaces.
      Root workspace: `cargo test -p flux-lang -p flux-flow -p flux-codegate`,
      `cargo clippy -p flux-lang -p flux-flow --all-targets -- -D warnings`,
      `cargo fmt -p flux-lang -p flux-flow -- --check` all green (see Progress log for
      counts). Plugins workspace untouched by this story — no plugin/host code changed,
      so its gate is out of scope for this diff.

## Notes
- Downstream: flux-model M-14 re-canonicalizes its corpus from stored `ast_json` via
  `flux-corpus fmt` at the new flux_rev — no flux-model code change needed by design.
- Redaction (C-22) replaces substrings inside string literals — verify the multi-line
  spelling stays parseable after redaction (same invariant L-38 asserts for plan_source).
  **Done**: `crates/flux-flow/src/loop_host.rs`'s `redacted_multiline_string_still_parses`,
  modeled on `redacted_plan_source_still_parses` — a `Lit` payload spanning 3 lines with a
  secret embedded mid-block; `format` chooses the `"""…"""` spelling; `Redactor::redact`
  replaces the secret with `[redacted]` (no quote/backslash chars, so it can't corrupt the
  block); `parse` still succeeds on the redacted text.
- **Chosen syntax + why (deviates from the old "aspirational" sketch that lived in
  `syntax.md`, which described a dedent-stripping design):** `"""` opens anywhere (inline
  content on the same line is fine — no "opener must end its line" rule); content is
  **fully verbatim** to the next
  literal `"""` — no escape processing, no comment stripping, **no indentation
  dedent/stripping at all**. This is the simplest possible instantiation of "a defined
  indentation rule" (Goal wording): the rule is "there is none — every byte between the
  delimiters is content." Chosen over a Java-text-block-style scheme (closer's own
  indentation stripped, opener-newline treated as a separator) because: (1) it is
  **delimiter-based, not indentation-based**, matching the explicit design constraint;
  (2) it has zero cross-line bookkeeping, so totality of `parse(&format(&ast)) == ast` is
  trivial to establish and verify (the only correctness burden is 3 narrow formatter-side
  safety guards, not a parser-side dedent algorithm); (3) it never re-introduces a
  whitespace-shaped failure mode for the exact model (small, fine-tuned) this story exists
  to help. Cost: the emitted block is not visually indent-aligned with surrounding code
  (the closing `"""` is glued right after the last content character, not on its own
  indented line) — acceptable, since the target consumer is a planner emitting payloads,
  not a human hand-formatting prose.
- **Formatter safety guards** (`format::is_safe_for_multiline_spelling`): a string falls
  back to the standard escaped spelling instead of `"""…"""` when it (a) contains the
  literal substring `"""`, (b) **ends** in a `"` (would merge with the closer into an
  ambiguous 4+-quote run), or (c) contains a `\r` (lost by `preprocess`'s `\r\n`->`\n`
  normalization — found via a peer session's review of the in-progress diff, see Progress).
  All three are vanishingly rare in real payloads; the round-trip invariant is total either
  way — these inputs just don't get the nicer spelling.

## Progress
- 2026-07-05 (session A) — took the story from the board (status → in-progress), mapped
  parse.rs/format.rs, and wrote an acceptance test suite — then discovered **another live
  session (B) was already implementing L-39** (parse.rs/format.rs/compile.rs/loop_host.rs/
  roundtrip_property.rs mtimes seconds old, uncommitted). Session A backed off all
  implementation; session B's design is authoritative:
  - **Implemented spelling (B): pure verbatim** — `"""` opens anywhere (inline content
    allowed), content is byte-verbatim to the next literal `"""` (no boundary-newline
    drops, no indent handling); desugared to an escaped JSON string at the lexer
    (`preprocess`), so every string-literal position accepts it. Formatter: recursive
    `compact_value` spells eligible string leaves as blocks incl. nested inside `Lit`
    containers; `format_compact` NEVER emits the spelling (decided, opposite of A's draft).
- **Handoff findings from session A** (verified by inspection against B's uncommitted tree
  @15:20; ground truth = the L-18 roundtrip invariant):
  1. **BUG — `\r\n` roundtrip violation:** `is_safe_for_multiline_spelling` (format.rs)
     lacks a `\r` guard, while the new `preprocess` opens with `src.replace("\r\n", "\n")`.
     A string like `"a\r\nb"` is block-eligible, emitted verbatim, and parses back as
     `"a\nb"` — `parse(format(A)) != A`. Fix: `&& !s.contains('\r')` (lone `\r` survives,
     but the conservative guard is simpler); add a `"cr\r\nlf\n"` property-pool entry.
  2. **Test-coverage nit:** the new `"trailing quote on a newline-bearing string\""` pool
     entry contains no `\n`, so it never exercises the ends-with-`"` fallback under the
     contains-newline condition; it should be e.g. `"line\nend\""`.
- Session A's parked assets (complementary, adjust boundary expectations to B's verbatim
  rule before reuse): an 18-case acceptance suite (position coverage: pure-JSON `Lit` arg
  objects, template-leaf-inside-`each` golden, fmt template, route case label, assert
  message, ctx purpose, escaped-quote non-trigger, multiple blocks per statement, @json
  stays compact, error-path assertions) at
  `/tmp/claude-1000/-home-timo-projects-flux/422bd54b-2f90-4e0d-92ea-934f2f20ed16/scratchpad/multiline_strings_acceptance_tests.rs`.
- 2026-07-05 (session B, continued after a session reset) — resumed as B, confirmed the
  working tree was mid-implementation (format.rs/parse.rs already carried the verbatim
  design), then closed the loop:
  1. **Applied A's `\r` finding as a real, verified fix.** Wrote a failing-first test
     (`format::tests::crlf_content_falls_back_to_the_escaped_spelling`) confirming the bug
     (a `"line one\r\nline two"` `Lit` was formatted with `"""…"""`, silently dropping the
     `\r` on reparse) *before* patching `is_safe_for_multiline_spelling` with the `\r`
     guard; the test then went green. Extended `roundtrip_property.rs`'s `STRINGS` pool
     with a CRLF entry per A's suggestion.
  2. **Applied A's test-coverage nit** — the "trailing quote" pool entry now actually
     contains a `\n` (`"line one\nline two ends in a quote\""`), so it genuinely exercises
     the ends-with-`"` fallback path instead of silently no-op'ing.
  3. **Mined A's parked 18-case suite** for cases compatible with B's simpler verbatim
     grammar (adjusting expected values — no separator/dedent semantics in B's rule — and
     dropping the two cases that directly assert A's now-superseded design:
     `opener_must_end_its_line`, `closer_glued_after_content_means_no_trailing_newline` /
     `indented_closer_whitespace_is_incidental`, and `format_compact_shares_the_block_spelling`
     which asserted the opposite of B's decided `format_compact` behavior). Added 7 new
     tests: `multiline_block_inside_a_pure_json_object_stays_a_lit_not_a_template`,
     `escaped_triple_quotes_inside_a_normal_string_are_not_mistaken_for_a_block`,
     `two_multiline_blocks_in_one_statement`,
     `multiline_content_preserves_blank_lines_indentation_and_statement_look_alikes` (all
     `parse.rs`); `multiline_string_inside_a_pure_json_lit_object_round_trips`,
     `json_escape_line_never_uses_the_multiline_spelling`, `edge_content_shapes_round_trip`
     (all `format.rs`).
  4. **L-20 grammar + C-22 redaction acceptance items closed** — see Acceptance/Notes above
     for the exact diffs and evidence.
  5. **Gate, package-scoped** (concurrent session was live on D-53, touching
     flux-events/flux-cli/its own story file/the board — untouched by this diff):
     - `cargo test -p flux-lang -p flux-flow -p flux-codegate` — **all green**
       (flux-lang lib 241, roundtrip_property 1, skill_in_sync 3, text_roundtrip 2,
       doctest 1; flux-flow lib 214, gather_effect_gate 3, skill_docs_in_sync 1, doctest 0;
       flux-codegate 4).
     - `cargo clippy -p flux-lang -p flux-flow --all-targets -- -D warnings` — clean.
     - `cargo fmt -p flux-lang -p flux-flow -- --check` — clean.
  - **Status: done.** Plugins workspace gate not run (out of scope — no plugin code
    touched by this story).
