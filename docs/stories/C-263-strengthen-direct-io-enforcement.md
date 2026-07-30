---
id: C-263
title: "Make direct-I/O enforcement structural and cover every production model-facing pack"
pillar: Core
status: done
priority: 8
epic: adversarial-review-remediation-2026-07-30
design: docs/designs/adversarial-review-remediation-2026-07-30.md
areas: [flux-codegate, flux-eval, flux-tools]
note: "MEDIUM assurance — the lexical guard excludes flux-eval under a premise contradicted by production registration"
---

# Make direct-I/O enforcement structural and cover every production model-facing pack

## Goal

Replace the hand-maintained three-crate claim with a gate derived from the production tool catalog
and strong enough to see imports, aliases, and newly introduced direct I/O APIs.

## Acceptance

- [x] The set of model-facing implementation crates is derived from production registration or a
      single exhaustively checked classification; `flux-eval` is included.
- [x] Failing-first fixtures cover imported/aliased filesystem, process, socket, HTTP, and database
      opens that the current fixed-text patterns miss, including local callable aliases, plus a
      direct call in `flux-eval`.
- [x] Enforcement uses syntax/HIR/dependency-boundary evidence rather than depending only on raw
      spellings; legitimate host/broker exceptions remain explicit and reasoned.
- [x] CI runs one named guard and its self-test, with no weaker duplicate silently passing over a
      narrower crate set.
- [x] Architecture docs state the actual scope and proof without claiming more than the gate proves.
- [x] Codegate/direct-I/O tests and the standard gate are green.

## Progress

- 2026-07-30 — replaced the shell tokenizer with a `syn` AST gate in `flux-codegate`. It resolves
  direct filesystem/process/socket/HTTP/database constructions through imports, renamed modules and
  types, type aliases, multiline calls, and parsed `cfg(test)` items.
- 2026-07-30 — one exhaustively reviewed classification now covers the first-party production
  operation packs: tools, web, capabilities, eval, cognition, flow, orchestration, and app runtime.
  `flux-eval` is explicitly included; its harness-owned temporary/result IO and the web/SQLite
  broker/store boundaries now carry call-local reasoned exceptions.
- 2026-07-30 — `scripts/check-no-direct-io.sh` is only the CI entry point for the codegate tests. It
  contains no crate list and no weaker lexical fallback; its self-test exercises every API family,
  aliases, test exclusion, and reason validation.
- 2026-07-30 — architecture prose describes the actual syntax-level proof. Codegate, wrapper
  self-tests/tree scan, affected crate tests, and scoped clippy with `-D warnings` are green.
- 2026-07-30 — the closure review demonstrated that `let call = forbidden::open; call(...)` escaped
  both the model-facing direct-I/O scan and the workspace process-construction scan. Both visitors
  now track lexical callable bindings and shadowing; the self-test covers every forbidden family.

## Notes

- Evidence: primary finding 2's assurance component and review B finding 6; follow-up to completed
  C-194 rather than a claim that C-194 delivered no value.
