---
id: C-739
title: "Acceptance criteria are addressable and carry their own verification handle"
pillar: "Core"
status: backlog
priority: 2
epic: delivery-is-verified
areas: [flux-cli]
---

# Acceptance criteria are addressable and carry their own verification handle

## Goal

Acceptance criteria are anonymous bullets, so nothing can reference one. A worker cannot report
evidence per criterion, review cannot be scoped to one, and a partially-satisfied story has no
representation — the only states are "all ticked" and "not done".

Kiro backlinks `_Requirements: 1.1_` and Spec Kit uses `FR-001` for exactly this reason. Our
checkboxes are most of the way there; they need stable ids. And C-723 already gropes toward a
verification handle by scraping backticked symbols out of prose — a criterion should simply name the
command that proves it.

## Acceptance

- [ ] Each criterion carries a stable id, allocated once and never renumbered, that a handoff, a
      review finding and a doctor report can all cite.
- [ ] A criterion may declare its own verification handle — the exact command, test name or
      observable artifact that proves it — and that handle is what evidence is checked against.
- [ ] Coverage is computable: which criteria are claimed, by which commit, with what evidence.
- [ ] C-723's `acceptance_artifacts` prefers a declared handle over scraping backticks from prose,
      and says which it used.
- [ ] Existing stories without ids keep working. This is additive; a story is not invalid for
      predating it.
- [ ] Regression test: a worker's handoff cites a criterion id, and a criterion whose declared
      verification did not run is reported as unproven rather than counted as satisfied.
