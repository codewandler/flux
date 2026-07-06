---
id: A-43
title: Stream plan skeletons on the OpenAI wire too — surface tool-args deltas as ToolInputDelta
pillar: Agent
status: backlog
epic: multipass-agent-loop
note: "L-23 residual: the Messages codec forwards input_json_delta as Chunk::ToolInputDelta so plan skeletons stream live; the OpenAI-wire codec (plain openrouter, codex — bedrock is Messages) accumulates tool args internally and never surfaces them — no skeleton there, same as pre-L-23"
---

# Stream plan skeletons on the OpenAI wire

## Goal
L-23 added `Chunk::ToolInputDelta` and wired it in the shared Messages codec; the OpenAI-wire
codec (`crates/flux-providers/src/openai.rs` — plain `openrouter`, `openai`, `codex`) still
accumulates streaming tool arguments internally and emits only the completed block, so plan-mode
skeleton rendering is silent on those providers. Surface the same additive chunk from the
OpenAI-wire delta handling so the L-23 render works wire-independently.

## Acceptance
- [ ] The OpenAI-wire codec yields `Chunk::ToolInputDelta` from its tool-args delta handling
      (mirroring `messages/mod.rs`); failing-first codec test.
- [ ] A compile-seam test proves skeleton headlines stream on an OpenAI-wire-shaped script.
- [ ] Stream-resilience invariants intact (A-33/A-34 tolerance unchanged); gate green.

## Progress
- 2026-07-06 filed — the residual recorded in L-23's Progress.

## Notes
- Bedrock speaks Messages and is covered already; codex WS re-envelopes through the Responses
  codec — verify where its tool-args deltas surface before assuming one edit covers both OpenAI
  shapes.
