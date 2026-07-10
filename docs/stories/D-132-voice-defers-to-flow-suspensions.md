---
id: D-132
title: Realtime/voice driver defers to flow suspensions (flow-driven voice)
pillar: Agent
status: backlog
epic:
design:
note: "downstream ask (ai-agent-platform R-20 voice half): a D-131 flow-driven session speaks its authored prompts over the realtime/voice provider and resumes on caller input"
---

# Realtime/voice driver defers to flow suspensions (flow-driven voice)

## Goal
Extend the D-131 flow-driven session mode to the **realtime/voice provider**: when a flow drives the
session, the voice driver **speaks the flow's authored prompts** (instead of letting the realtime
model improvise) and **resumes the suspension on caller input** — classic-IVR-shaped determinism over
the existing voice stack. Today `flux_flow::voice::VoiceSessionDriver` assumes the model owns the
conversation; a parked flow suspension has no voice pathway.

## Acceptance
- [ ] A `mode`/entry on the voice driver runs a D-131 flow-driven session: the authored prompt at each
      suspension is spoken (TTS via the realtime channel), the caller's reply resumes the flow, and
      model cognition runs only where the flow calls it. Failing-first: a two-`await` flow over the
      voice driver produces the two authored prompts with zero planner invocations (transcript-level
      assertion — no live audio needed, mirroring the existing driver tests).
- [ ] Flow completion maps to the driver's existing terminal controls (hangup / handoff) the same way
      text completion returns the flow result.
- [ ] Usage capture (realtime usage) and the event/turn projection work for flow-driven voice sessions
      exactly as for model-driven ones. Gate green.

## Progress
- 2026-07-10 — filed from the ai-agent-platform flows-arc design (R-20 covers text + voice together,
  per their product decision). Depends on D-131.

## Notes
- The downstream consumer keeps its RTVBP boundary + audio bridge; this ask is only about the driver
  honoring a flow suspension as the source of the next assistant utterance.
- Downstream note: their `channel-rtvbp` currently constructs the backend before
  `session.initialize` — they track a deferred-construction change (R-18) that may land first on
  their side.
