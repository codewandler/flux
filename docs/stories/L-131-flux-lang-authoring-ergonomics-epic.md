---
id: L-131
title: "Flux-Lang authoring ergonomics — typed protocols, concise data and control (epic)"
pillar: Language
status: backlog
epic: flux-lang-authoring-ergonomics
design: docs/designs/flux-lang-authoring-ergonomics.md
areas: [flux-lang, flux-lsp]
note: "EPIC — provider-neutral types, task contracts, recovery, reusable values, fan-out, collecting loops, structured context, and explicit optional results"
---

# Flux-Lang authoring ergonomics — typed protocols, concise data and control (epic)

## Goal

Make substantial Flux programs read like the protocol they implement instead of repeated
JSON parsing, assertions, object construction, duplicated task branches, and hand-maintained loop
state. Add only provider-neutral, domain-neutral primitives, and keep effects, bounds, failure, and
provenance visible.

## Acceptance

- [ ] L-132 establishes structural records, finite enums/literal unions, optional fields, and a
      bounded refinement vocabulary shared by the analyzer and runtime.
- [ ] L-133 and L-134 make task results typed and malformed external output recoverable through an
      explicit, bounded repair path distinct from fatal invariants.
- [ ] L-135 and L-136 remove repeated data-construction ceremony with pure local functions and one
      canonical multiline/spread/update vocabulary.
- [ ] L-137 and L-138 express bounded data-driven parallel work and refinement history without
      hiding cancellation, ordering, failure, or loop budgets.
- [ ] L-139 and L-140 make task inputs/context and optional/recoverable outcomes structural values.
- [ ] Every child story ships a provider-neutral and domain-neutral example; none adds a vendor,
      blog, review, deployment, or incident-specific language primitive.
- [ ] The design's generic three-check refinement fixture preserves observable behavior while
      materially reducing repeated task/parse/assert/return scaffolding.
- [ ] All syntax changes satisfy `crates/flux-lang/AGENTS.md`: failing-first coverage, formatter
      round-trip, regenerated goldens/docs, and every maintained editor grammar mirror.
- [ ] Approval, confinement, cancellation, trace integrity, and execution budgets are unchanged or
      strengthened.

## Progress

- 2026-08-05: Epic and nine child stories proposed after reviewing the ceremony in a substantial
  Flux content pipeline. The extracted capabilities and examples were deliberately generalized so
  the language design does not inherit that pipeline's domain.

## Notes

- Full proposal and illustrative combined program:
  [flux-lang-authoring-ergonomics](../designs/flux-lang-authoring-ergonomics.md).
- The examples below are a syntax target, not accepted grammar. Each child records its exact
  syntax, AST, lowering, compatibility, and mirror plan before implementation.
- Generic end-state shape:

  ```flux
  checks = [
    { stage: "correctness", role: "review-correctness" },
    { stage: "safety", role: "review-safety" },
    { stage: "clarity", role: "review-clarity" }
  ]

  repeat 3 as iteration, until: cycle.clear -> history
    parallel settled each check in checks -> reviews
      task(role: check.role, input: { candidate, stage: check.stage }) as Review<check.stage>
        repair 1 with validation_error
    cycle = summarize(iteration, reviews)
    yield cycle
  ```

- L-102 remains authoritative for canonical spelling and L-113 for parser/runtime hardening. L-133
  builds on, rather than duplicates, D-05's runtime structured-output seam.
- Child stories: L-132 … L-140.
