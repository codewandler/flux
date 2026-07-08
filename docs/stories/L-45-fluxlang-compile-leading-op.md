---
id: L-45
title: `fluxlang compile` must accept modules with a leading `op` (parity with `flow run`)
pillar: Language
status: done
epic: beta-hardening
design: docs/designs/beta-hardening.md
note: "F-013: the developer `fluxlang compile` CLI rejects a module starting with `op` that `flux flow run` executes successfully — the two front-ends disagree on valid module syntax"
---

# `fluxlang compile` must accept modules with a leading `op`

## Goal
The developer CLI `fluxlang compile` rejects a module whose first declaration is an `op`, while
`flux flow run` compiles and executes the same module fine. Two front-ends over the same language
must agree on what is a valid module — bring `fluxlang compile` to parity.

## Why (evidence)
- Beta F-013: "The developer CLI rejected a module that `flux flow run` executed successfully."
  (Module with a leading `op`.)

## Acceptance
- [ ] `fluxlang compile <module-with-leading-op>` succeeds on any module `flux flow run` accepts.
- [ ] Failing-first test: a fixture module beginning with `op …` compiles via the same entry point
      `fluxlang compile` uses (currently rejected).
- [ ] Root-cause noted: whether `fluxlang compile` used a stricter/older parse entry than
      `flow run`, and the two are unified (single parse path) rather than patched in two places.

## Progress
- 2026-07-08 **DONE.** Root cause confirmed: `fluxlang compile` called the flow-only
  `flux_lang::parse::parse` (requires a `flow` header on line 0), while `flux flow run` uses
  `Module::parse_str` → `parse_program` (dispatches `op`/`agent`/… as well). Unified `compile` onto
  `Module::parse_str` (shared `compile_src`), serializing `Module::Flow` → `DraftAst` (byte-identical
  for bare flows, preserving the round-trip) and `Module::Program` → `Program`. Test:
  `compiles_a_module_with_a_leading_op`.

## Notes
- Likely a divergent parse/compile entry point between the `fluxlang` dev binary and the `flux flow
  run` path; prefer converging on one module-parse function over duplicating the fix.
- Epic: [beta-hardening](../designs/beta-hardening.md).
