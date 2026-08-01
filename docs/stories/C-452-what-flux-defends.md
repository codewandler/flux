---
id: C-452
title: "Write down what flux defends rather than closes — so nobody 'fixes' the envelope to win a rubric"
pillar: Core
status: ready
priority: 5
design: docs/designs/pi-comparison-remediation.md
epic: pi-comparison-remediation
areas: [docs]
note: "⚠ the story that keeps the rest of the epic honest. Several axes flux scores lower on are things flux CHOSE — 38 crates and a mandatory envelope, an extension surface that is deliberately not replaceable in-process. Closing them raises the score and destroys the product"
---

# The trade-offs, argued once, in writing

## Goal

State which comparison findings flux **will not** close, and why — so the reasoning survives the next
reader, the next review, and the next person tempted to optimise a number.

## Why this is a story and not a comment

Three axes from the Pi comparison are deliberate trade-offs:

- **Performance / complexity — 6.5 vs 7.5.** The cited reason is 38 crates and a mandatory envelope.
  ⚠ Removing the envelope raises the score and destroys the thing flux is for.
- **Maximum extension freedom — Pi.** *"In-process TypeScript can replace nearly every layer."* The
  review's very next sentence: *"This is also why it is not a security boundary."*
- **Ecosystem — 7.0 vs 8.0.** 81,617 stars against zero. **Not closable by code**, and the review says
  it *"lowers integration discovery risk, not execution risk."*

⚠ Without this written down, a future contributor reads a rubric, sees flux behind, and closes a gap
that was a feature. That is a real failure mode and it is cheap to prevent.

## Acceptance

- [ ] Each defended trade-off stated with **what it buys and what it costs** — not a defence, a
      reckoning. A trade-off with no stated cost reads as denial.
- [ ] ⚠ Every finding in the review is in **exactly one** of three buckets — *close it* · *defend it* ·
      *not code* — and the buckets are complete. A finding in none of them is one nobody decided about.
- [ ] Lives where a contributor will meet it before proposing a change. `docs/vision.md` already carries
      the principles that decide ties and is the likeliest home; ⚠ do not create a fourth document that
      says what three already imply.
- [ ] ⚠ **Honest about what is genuinely behind.** A page that defends everything is a page nobody
      believes. C-444 (SDK defaults), C-445 (interactive confinement), C-446 (Windows) and C-448
      (cancellation) are **real gaps**, and this page should say so and point at them.
- [ ] The register matches `vision.md`'s own — it states the improvement-loop pillar is *"currently
      aspirational, and this document says so honestly."*
- [ ] Full gate green.

## Notes

- ⚠ Not a positioning page and not a comparison table. [C-429](C-429-the-recipes-surface-and-positioning.md)
  already decided that public positioning argues from the architecture and names no competitor. This is
  **internal** — its audience is a contributor with a rubric in hand.
- [C-451](C-451-the-head-to-head-benchmark.md) makes this page much stronger: *"the envelope costs X%"*
  beats *"the envelope is worth it"*.
- ⚠ The review's own Bottom Line is the best short statement of the trade-off and worth quoting rather
  than paraphrasing: *"choose Flux when the runtime must remain the authority after the model, prompt and
  workflow have spoken."*

## Progress
- Filed 2026-08-02 from the Pi comparison.
