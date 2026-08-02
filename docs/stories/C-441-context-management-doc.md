---
id: C-441
title: "A context-management section — the question every user asks, answered nowhere"
pillar: Core
status: in-progress
priority: 5
design: docs/designs/docs-completeness.md
epic: docs-completeness
areas: [website, docs]
note: "⚠ the mechanism is implemented and its entire user-facing documentation is ONE ROW in a config table: `FLUX_COMPACT_CHARS`. `context-packs.md` (the Flux-Lang ctx construct) and `project-context.md` are real and answer different questions, which is why the gap looks covered from the inside"
---

# What happens when the conversation gets long

## Goal

A user can answer, from the docs: what fills the context, what flux does when it fills, what is lost,
what is kept, how to control it, and what it means for the session afterwards.

## Why it looks covered and is not

- `website/docs/language/context-packs.md` documents `ctx` — the **Flux-Lang** construct.
- `website/docs/agent/project-context.md` documents **project** context.
- `website/docs/reference/config.md:507` documents `FLUX_COMPACT_CHARS` as one row: *"Character
  threshold that triggers history compaction."*

Three real pages, none of which answers *"what happens when my conversation gets long?"* — and their
existence is exactly why the gap is invisible from inside the project.

⚠ **This may be findability as much as coverage.** `FLUX_COMPACT_CHARS` *is* documented — in a
500-line config table, where nobody searching for a concept will meet it. A concept page that links to
the knob is the fix; a second copy of the knob is not.

## Acceptance

- [ ] ⚠ **Blocked on [C-443](C-443-zero-compacted-rows.md)** until it is known whether compaction
      actually fires. A page describing behaviour nobody has observed is documentation of an intention,
      and a 112k-event store contains zero `Compacted` rows.
- [ ] The page answers all six: what fills the context · what happens when it fills · what is lost ·
      what is kept · how to control it · what it means afterwards.
- [ ] ⚠ **It says plainly that compaction *replaces* history in the durable log** — `EventKind::Compacted
      { messages }`. That changes what a session is for `flux replay`, `flux export` and anything
      reconstructing a run, and a user who does not know it will be surprised at the worst moment.
- [ ] The relationship to the neighbours is stated, so the three pages stop looking like alternatives:
      Flux-Lang's `ctx`, project context, and this.
- [ ] Every knob it names links to the config reference rather than restating it — one source of truth
      for the value, one place for the concept.
- [ ] ⚠ Honest about what is *not* managed. If flux does not do something users expect from other
      harnesses — automatic summarization, per-tool budgets, retrieval — say so rather than leaving the
      reader to infer it exists.
- [ ] Where the page states behaviour, it is behaviour the code does. If C-443 finds compaction rarely
      fires, the page says so.
- [ ] Full gate green, including the website checks.

## Notes

- The peer-docs audit ([C-442](C-442-peer-docs-gap-audit.md)) will likely surface neighbours worth
  covering in the same section — token budgets, session boundaries. Do not wait for it; do not duplicate
  it either.
- ⚠ The register to match is `vision.md`'s: it states the improvement-loop pillar is *"currently
  aspirational, and this document says so honestly."* A context page that oversells is worse than none,
  because context handling is exactly what an evaluator stress-tests first.

## Progress

- Filed 2026-08-02 at the owner's request.
