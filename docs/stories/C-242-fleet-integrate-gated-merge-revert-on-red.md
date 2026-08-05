---
id: C-242
title: "`fleet.integrate` and explicit `fleet.apply` — one final gate, no automatic publication"
pillar: Core
status: ready
priority: 48
epic: fleet-loop
design: docs/designs/native-board-fleet-cli.md
areas: [flux-tools, flux-runtime, flux-cli]
note: "Decision 0010 publication fence — green leaves local fleet/<wave>; apply revalidates and merges; neither command pushes"
---

# `fleet.integrate` and explicit `fleet.apply` — one final gate, no automatic publication

## Goal

Assemble accepted story commits in dependency order, run one unskippable repository gate on the final
tree and make both publication-on-red and implicit publication impossible.

## Acceptance

- [ ] Failing-first tests cover a combined-only failure and a green wave. The gate runs exactly once
      after the last accepted commit; missing/unrunnable gate is red, never pass.
- [ ] Inputs are capped at ten and carry `BoardRef`, writer/worktree identity, exact commit,
      dependency order, typed write set and targeted evidence. Duplicate stories/writers and unsafe
      overlap refuse before integration.
- [ ] Conflicts name story and files, append recoverable evidence and preserve the candidate history;
      no reset, rewrite, competing writer, partial done state or automatic retry occurs.
- [ ] Red records the exact candidate SHA and leaves planning items non-done. Green records a local
      `fleet/<wave>` branch eligible for apply. Neither state pushes or opens a pull request.
- [ ] `flux fleet apply WAVE` requires the recorded green gate, revalidates base/revisions/cleanliness,
      merges locally in repository order and records the result. It never pushes, releases or deploys.
- [ ] Human/JSON outputs expose candidate, gate evidence, conflicts and apply eligibility under
      C-547's typed errors and idempotency semantics.
- [ ] Accurate effects/access/intents and concrete permission subjects are pinned. Targeted tests
      pass; A-117's integrated fleet wave owns the full gate.

## Notes

- C-238 and C-241 are delivered prerequisites. C-244 handoff supplies accepted story commits.
