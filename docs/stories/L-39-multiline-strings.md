---
id: L-39
title: Multi-line string literals in flux-lang — kill the escaped-single-line-JSON wall
pillar: Language
status: ready
priority: 2
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
- [ ] Grammar + parser: multi-line string accepted in every string-literal position;
      spec'd indentation/termination rules documented in the language docs.
- [ ] Formatter: `format::format` emits the multi-line spelling iff the string contains
      a newline (deterministic; no config); `format_compact` behavior decided + tested.
- [ ] Failing-first roundtrip tests: property/unit tests extending the L-18 suite —
      newline-bearing strings survive `parse ∘ format` fingerprint-stable; goldens for
      nested cases (multi-line string inside an object template inside `each`).
- [ ] The planner text grammar (L-20 `build_text_grammar`) teaches the new spelling;
      `text_grammar_examples_parse_and_match_the_json_arm` still green.
- [ ] Full gate green in BOTH workspaces.

## Notes
- Downstream: flux-model M-14 re-canonicalizes its corpus from stored `ast_json` via
  `flux-corpus fmt` at the new flux_rev — no flux-model code change needed by design.
- Redaction (C-22) replaces substrings inside string literals — verify the multi-line
  spelling stays parseable after redaction (same invariant L-38 asserts for plan_source).
