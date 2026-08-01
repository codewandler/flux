---
id: C-433
title: "A distilled sequence of clicks is automation, not a test — what the agent verified has to survive freezing"
pillar: Core
status: backlog
design: docs/designs/explore-then-freeze.md
epic: explore-then-freeze
areas: [flux-flow, flux-web]
note: "⚠ the epic's worst failure mode: a green suite that asserts nothing is worse than no suite, because it is trusted. 'Test the happy path' means checking something; a frozen click sequence proves only that the clicks did not error"
---

# A green test that checks nothing

## Goal

The frozen script asserts what the exploration actually verified, and goes red when that stops being
true.

## Why this is separable from the distiller

[C-430](C-430-distil-an-exploration-into-a-flow.md) can succeed completely — emit a readable flow with
the backtracking removed, durable locators, no credentials — and still produce something that is not a
test. A sequence of clicks that runs to completion proves the clicks did not error. It does not prove
the module works.

⚠ **And the failure is silent in the worst direction.** A script with no assertions goes green forever,
including after the feature it was written for is broken. A suite like that is worse than no suite,
because people trust it and stop looking.

## Acceptance

- [ ] **Failing-first**: a test freezing an exploration that verified a page condition, then mutating
      the target so that condition is false, asserting the frozen script **fails** — failing at the
      merge base (where it passes, because nothing asserts).
- [ ] What the agent checked during exploration is identified and carried into the frozen script.
      ⚠ Note the hard part: the agent's verification may be *implicit* — it read the digest and
      continued because the page looked right. Decide whether an implicit check becomes an assertion,
      is prompted for at freeze time, or is out of scope; do not leave it to fall out of the
      implementation.
- [ ] Assertions are expressed against the semantic page model — roles, names, states from the digest —
      consistent with [C-431](C-431-durable-locators.md)'s locators. Two different notions of "what the
      page says" would be one too many.
- [ ] ⚠ **A frozen script with no assertions is refused or loudly marked**, not written silently. This
      is the acceptance criterion that decides whether the epic ships a test tool or a macro recorder.
- [ ] Failure output names what was expected and what was found. An e2e failure that does not say
      what changed is a re-exploration, which is the cost this epic exists to remove.
- [ ] Full gate green.

## Notes

- Depends on C-430 existing; the interface with [C-431](C-431-durable-locators.md) matters more than
  the order.
- ⚠ The model's judgement of "the happy path" is inherited by the whole chain. If the agent succeeded
  by accident — clicked something that happened to work — the frozen script enshrines the accident.
  Assertions are the only place that becomes *detectable*, which is the real argument for this story.
- Worth considering: the exploration's own failures are evidence about what *should* fail. An agent
  that hit a validation error on a bad input learned a real property. Out of scope here, but if it
  falls out cheaply, record it rather than discarding it.
- `flux diff` (C-44) is the natural tool for "the UI changed, what broke" — a re-exploration diffed
  against the frozen script. Not this story, but the assertion vocabulary should not make it harder.

## Progress

- Filed 2026-08-01 with the explore-then-freeze epic.
