---
id: L-40
title: Re-run the emission A/B with the fine-tuned local model as the text arm
pillar: Language
status: backlog
epic: flux-planner-ship
design: docs/designs/flux-planner-ship.md
note: "the ONE pre-registered condition allowed to re-open L-20's keep-json decision: a model that natively speaks the text syntax; blocked on flux-model M-15 producing a candidate that passes the ship gate"
---

# Emission A/B re-run: fine-tuned text arm

## Goal
Wire the ollama-served fine-tuned planner (flux-model's ship candidate) as the text-arm
model behind the kept `FLUX_EMISSION=text` scaffold, and re-run the L-20 A/B on the same
task suite (`crates/flux-eval/assets/emission-ab/tasks.json`) with the same metrics —
plus with-one-repair-round numbers, since the loop's repair round is the production path.
Record the ship/no-ship decision against the pre-registered bar in the epic design doc.

## Acceptance
- [ ] The fine-tune is reachable as a flux provider (ollama speaks Anthropic Messages;
      model spec documented, e.g. `-m ollama/flux-planner`).
- [ ] A/B harness run: json arm (production model) vs text arm (fine-tune), same tasks,
      first-emission acceptance + repair-round counts + token costs, 3 seeds for the
      text arm.
- [ ] Decision recorded in `docs/designs/flux-lang-emission-ab.md` (follow-up section)
      and the epic design doc: re-open keep-json ONLY if the text arm decisively beats
      the measured 60% baseline (json's 93% is the reference ceiling).
- [ ] No production default changes in this story — measurement + wiring only.

## Notes
- Blocked by: flux-model M-15 (ship gate) — do not run against the research-licensed 3B.
- Cross-repo tracking (2026-07-06): all flux-model intents (corpus/training/ship-gate stories
  M-01..M-16 and the epic designs) are tracked in `../flux-model` (`docs/stories/`,
  `docs/designs/flux-planner-ship.md`); the `design:` link above resolves to a redirect stub.
  This story is the flux-side gate consumer and deliberately stays on this board.
- The L-20 scaffold was deliberately kept for exactly this story (projection-not-emission
  decision, 2026-07-04).
