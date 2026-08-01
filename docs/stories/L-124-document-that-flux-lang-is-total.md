---
id: L-124
title: "The docs never say that flux-lang is total, which is one of its most important properties"
pillar: Language
status: ready
priority: 12
epic: flux-lang-hardening
design: docs/designs/flux-lang-hardening.md
areas: [flux-lang, docs, website]
note: "nothing in the tree states it. `grep -rn 'Turing\\|terminates\\|totality'` over docs/, website/docs/ and crates/flux-lang/docs/ returns only flux-exchange 'terminates channels' and the roadmap's round-trip-totality note — a different sense of both words. The website's control-flow page says 'keep loops bounded' without saying what that buys"
---

# Say that flux-lang is total, and what that buys

## Goal

Explain, where a reader will meet it, that **flux-lang is not Turing complete — it is total**: every
authored Flux program terminates. That is a deliberate design decision with real consequences for
anyone running flux unattended, and right now the documentation never states it.

## Why this matters enough to be its own story

A reader coming from any general-purpose language will assume they can write an unbounded loop, go
looking for one, and conclude the language is missing a feature. The truth is the opposite: the
absence is the feature, and it is what lets the runtime promise that an authored flow cannot hang
the agent.

The property is bounded at **two independent levels**, and both belong in the explanation:

1. **The grammar has no unbounded loop form.** All three iteration constructs carry their bound in
   the syntax:
   - `repeat N` — a literal count;
   - `each` over a collection — bounded by the collection;
   - `loop { for_ms, every_ms, until }` — bounded by **wall-clock duration**, where `until` is an
     optional *early* exit rather than the only one.

   There is no `while cond` that runs until a condition flips.
2. **The runtime backstops it.** `DEFAULT_MAX_LOOP_ITERATIONS` (100,000) is charged per flow
   execution and shared across `repeat`/`each`/`loop` at every nesting depth (L-116), and recursive
   composite ops are capped by `DEFAULT_MAX_COMPOSITE_DEPTH` (L-81), so `op f() { call f() }`
   returns rather than recursing forever.

## Acceptance

- [ ] The property is stated where a language reader meets it — at minimum
      `website/docs/language/control-flow.md`, which today says "keep loops bounded" without saying
      what that buys, and `crates/flux-lang/docs/syntax.md`.
- [ ] **Both levels are explained**, not just the runtime budgets. "The grammar has no unbounded
      loop" is the stronger and more surprising half, and a reader who only learns about budgets will
      think the limit is a tunable rather than a language property.
- [ ] A short plain-language definition of Turing completeness and the halting problem, for readers
      who have not met the terms. One paragraph, not a CS lecture.
- [ ] ⚠ **The honest caveats are included, or the doc becomes an overclaim:**
      - totality here comes from *enforced bounds*, not from a type system that makes non-termination
        unrepresentable (the Coq/Agda approach) — remove the budgets and the depth cap and the
        expressiveness argument would have to be made again;
      - **the system is not bounded the way the language is.** The agent loop calls a model
        repeatedly and the model authors new flows; it has its own separate cap
        (`MAX_AGENT_LOOP_ITERATIONS`). "Authored Flux always terminates" is a statement about the
        language, not about what an agent running it will eventually do;
      - a composite op call re-enters with a fresh loop budget (L-116's documented boundary), so the
        bound is finite but is not a single whole-process ceiling.
- [ ] The numbers are **derived or cross-checked against the constants**, not retyped. Three stories
      have already moved these values (L-81, L-114, L-116); a doc that hardcodes 100,000 or 8 goes
      stale the next time. If a generated block or a test that reads the constants is proportionate,
      prefer it — `website_in_sync.rs` already establishes that pattern for other generated docs.
- [ ] Full gate green, including `cargo test -p codewandler-flux-lang --test website_in_sync` if the
      website mirror is touched.

## Notes

- Prior art for where this belongs: `docs/concepts.md` defines the vocabulary the documentation uses
  and would be a reasonable second home for the one-line version.
- ⚠ Do not confuse this with the other two senses of the words already in the tree:
  `docs/ecosystem.md` and `docs/concepts.md` use "terminates" for flux-exchange terminating
  *channels*, and `docs/roadmap.md` uses "totality" for round-trip totality in the parser. Both are
  unrelated. Consider whether the new text should disambiguate.
- The constants and their reasoning live at `crates/flux-lang/src/runtime.rs`
  (`DEFAULT_MAX_LOOP_ITERATIONS`, `DEFAULT_MAX_COMPOSITE_DEPTH`) — L-116 wrote a long doc comment
  there that is most of the argument already, and is the best source to draw from.
- L-114's depth guard is the parse/lower-time companion to this: deep nesting yields a bounded error
  rather than aborting. Worth a sentence, since a reader may wonder what happens to a program that
  is merely *large* rather than non-terminating.

## Progress

- Filed 2026-08-01, from a user question — "is flux-lang Turing complete, and what does that mean?"
  Answering it required reading the runtime constants and the AST, which is the evidence that the
  documentation does not answer it.
