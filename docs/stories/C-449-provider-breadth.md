---
id: C-449
title: "Provider breadth — Pi constructs 39 variants; decide whether flux competes on that axis"
pillar: Core
status: ready
priority: 9
design: docs/designs/pi-comparison-remediation.md
epic: pi-comparison-remediation
areas: [flux-providers, docs]
note: "⚠ a DECISION before a build. The review calls provider breadth `a major product advantage` for Pi (39 variants) while scoring both 9.0 on the providers axis — so this is adoption surface, not a defect. Adding providers is cheap to start and expensive to maintain"
---

# How many providers is the right number?

## Goal

Decide whether flux competes on provider breadth, and act on the decision rather than drifting.

## The finding

> *"Provider breadth is a major product advantage: the registry constructs 39 provider variants in the
> pinned tree."*

⚠ Both harnesses score **9.0** on the providers/context/sessions/cancellation axis, and flux is credited
with *"resilient codecs."* So this is not a quality gap — it is a **catalogue** gap, and it is a real
adoption lever: the provider someone already uses being absent is a first-five-minutes bounce.

## Acceptance

- [ ] Count what flux supports today, and against what interface — a number, not an impression.
- [ ] ⚠ **Decide the strategy explicitly**, because the options have very different costs:
      (a) breadth in-tree — every provider is maintenance and a CI surface forever;
      (b) an OpenAI/Anthropic-compatible path that covers most of the tail without naming each;
      (c) providers as connectors/plugins, so breadth lives outside the release closure.
      ⚠ Option (c) fits the ecosystem split flux already made — *flux knows kinds, never vendors* — and
      should be the default answer unless there's a reason against it.
- [ ] Whatever is chosen, the docs say **how to reach a provider flux does not name**, which is the
      question a bouncing evaluator actually has.
- [ ] ⚠ Do not add providers one at a time without the decision. Nine half-maintained providers is worse
      than three good ones plus a documented compatible path.
- [ ] Full gate green.

## Notes

- Memory of this repo's own practice: OpenRouter already fronts many models through one provider, which
  is evidence for option (b)/(c) rather than (a).
- Feeds the docs-completeness epic: "which providers, and how do I add one" is exactly the kind of
  obvious-to-everyone-else topic [C-442](C-442-peer-docs-gap-audit.md) is auditing for.

## Progress
- Filed 2026-08-02 from the Pi comparison.
