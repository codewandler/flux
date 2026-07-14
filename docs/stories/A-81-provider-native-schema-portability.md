---
id: A-81
title: Make surfaced operation schemas portable across native providers
pillar: Agent
status: backlog
priority: high
note: "Gemini rejects valid JSON Schemas that omit array items or name required fields absent from properties; normalize or reject before the wire without weakening host validation"
---

# Make surfaced operation schemas portable across native providers

## Goal
Ensure every operation schema Flux advertises through a provider-native tool interface is accepted
by that provider, while retaining the original complete schema for host-side validation and never
widening what an operation may receive.

## Acceptance
- [ ] A failing-first OpenRouter/Gemini codec fixture covers arrays without `items`, nested arrays
      without inner `items`, and `required` names absent from `properties`.
- [ ] Provider projection either produces the provider's supported schema subset deterministically
      or rejects an incompatible registered operation before making a paid request, naming the
      operation and exact schema path.
- [ ] The runtime still validates returned arguments against the operation's original full schema;
      projection cannot weaken authorization, approval, or dispatch validation.
- [ ] The A-78 support and Bitcoin-to-Slack Gemini repros no longer fail with
      `GenerateContentRequest.tools…parameters…items: missing field`.
- [ ] Cross-provider tests cover Anthropic, OpenAI/OpenRouter, Codex, and Gemini-facing projection.

## Progress
- 2026-07-14: filed from A-78 paired confirmation. OpenRouter Gemini support session `s_1356`
  selected a cognition-extended surface and returned HTTP 400 for many array properties missing
  `items` plus required/property mismatches. Slack session `s_1422` independently failed on
  `blocks.items`. The failures occur before model generation and are unrelated to intent latency.

## Notes
- The original tool schema remains the safety contract. Any provider-specific projection is a wire
  compatibility view only, analogous to native operation-name aliasing.
