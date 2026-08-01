---
id: C-451
title: "Nobody has measured either harness — every performance claim in the comparison is inferred from source"
pillar: Core
status: ready
priority: 6
design: docs/designs/pi-comparison-remediation.md
epic: pi-comparison-remediation
areas: [flux-eval, docs]
note: "⚠ the review's OWN first open question, and the one that would settle several arguments at once. It states plainly: `no task-quality benchmark, runtime benchmark, fuzzing` — so the 6.5-vs-7.5 performance score, and flux's own throughput ceiling claim, are both readings of code"
---

# Measure it, then argue

## Goal

Produce one head-to-head measurement, so the next comparison argues from numbers.

## Why this outranks most of the epic

The review's method line says it: *"two isolated source-level desk reviews using one shared nine-axis
rubric… **no task-quality benchmark, runtime benchmark, fuzzing**"*. Its first open question is:

> *"How do the two harnesses compare on the same model, repository, task corpus and approval posture for
> success rate, latency, token cost and operator interventions?"*

⚠ So **Performance/complexity 6.5 vs 7.5 is an inference**, and so is *"a real throughput ceiling"*
(C-447), and so is *"Pi parallelizes tool work"*. Nobody has run either. flux has `flux-eval` and an
improvement harness built for precisely this, which makes flux the cheaper side to measure first.

## Acceptance

- [ ] At least one measurement on **the same model, repository and task corpus**, reporting success
      rate, latency, token cost and operator interventions.
- [ ] ⚠ **The approval posture is held equal and stated.** Comparing flux-with-an-envelope against a
      harness with none measures the envelope, not the harness — and the envelope's cost is a legitimate
      thing to measure, but only if it is what you say you measured.
- [ ] Cold-start time, steady-state memory and sustained turn/tool throughput on the same host — the
      review's second open question, and cheaper than the first.
- [ ] ⚠ **Publish the result even if flux loses.** A benchmark run only until it flatters is worth less
      than none, and this repo's own register — `vision.md` calling a pillar *"currently aspirational,
      and this document says so honestly"* — is the standard to hold.
- [ ] The method is repeatable and recorded, so a later run is comparable rather than a fresh opinion.
- [ ] Full gate green.

## Notes

- ⚠ Beware the warm-cache trap this repo has already hit in A/B evaluation: a same-prompt second arm can
  read the first arm's cache. Alternate arm order.
- Feeds [C-447](C-447-the-per-engine-turn-mutex.md) directly — it cannot state a throughput target
  without a number, and this story produces one.
- Feeds [C-452](C-452-what-flux-defends.md): "the envelope costs X%" is a far stronger defence of the
  trade-off than "the envelope is worth it".

## Progress
- Filed 2026-08-02 from the Pi comparison.
