---
id: A-81
title: Make surfaced operation schemas portable across native providers
pillar: Agent
status: done
priority: 2
design: docs/designs/provider-native-schema-portability.md
note: "Gemini rejects valid JSON Schemas that omit array items or name required fields absent from properties; normalize or reject before the wire without weakening host validation"
---

# Make surfaced operation schemas portable across native providers

## Goal
Ensure every operation schema Flux advertises through a provider-native tool interface is accepted
by that provider, while retaining the original complete schema for host-side validation and never
widening what an operation may receive.

## Acceptance
- [x] A failing-first OpenRouter/Gemini codec fixture covers arrays without `items`, nested arrays
      without inner `items`, and `required` names absent from `properties`.
- [x] Provider projection either produces the provider's supported schema subset deterministically
      or rejects an incompatible registered operation before making a paid request, naming the
      operation and exact schema path.
- [x] The runtime still validates returned arguments against the operation's original full schema;
      projection cannot weaken authorization, approval, or dispatch validation.
- [x] The A-78 support and Bitcoin-to-Slack Gemini repros no longer fail with
      `GenerateContentRequest.tools…parameters…items: missing field`.
- [x] Cross-provider tests cover Anthropic, OpenAI/OpenRouter, Codex, and Gemini-facing projection.

## Progress
- 2026-07-14: filed from A-78 paired confirmation. OpenRouter Gemini support session `s_1356`
  selected a cognition-extended surface and returned HTTP 400 for many array properties missing
  `items` plus required/property mismatches. Slack session `s_1422` independently failed on
  `blocks.items`. The failures occur before model generation and are unrelated to intent latency.
- 2026-07-14: failing-first
  `cargo test -p codewandler-flux-providers openrouter::tests::gemini_codec_ -- --nocapture`
  proved both gaps: nested array `items` remained absent and a closed required/property mismatch
  reached body construction instead of rejecting locally.
- 2026-07-14: both OpenRouter codecs now derive a model-specific cloned schema view. Exact
  rewrites cover unconstrained arrays, required properties governed by `additionalProperties`, and
  nullable unions, plus annotation-only multi-concrete unions through equivalent `anyOf` branches;
  every unsupported, widening, or malformed construct rejects before endpoint lookup with the
  operation and original RFC 6901 source path. The registered request/tool schema is never mutated,
  and returned calls still validate against the full original `ToolSpec`.
- 2026-07-14: hermetic cross-provider and zero-endpoint fixtures pass, including nested unsupported
  keywords, compound/nullable unions, closed objects, standalone nullable widening, inherited
  `additionalProperties` path provenance, non-string Gemini enum members, primitive/array roots, and a
  nullable root object. Exact positive coverage includes type-only `anyOf` nullability, safe
  multi-concrete unions, implied string-enum types, and nested unconstrained arrays. An inert credentialed
  probe containing `$defs`/`$ref` passed through both OpenRouter Chat and Messages wires without
  registering or executing an operation.
- 2026-07-14: rebuilt exact repros pass on Gemini 3.5 Flash. Support session `s_1439` cited all four
  retained sources with the expected facts; Bitcoin-to-Slack session `s_1440` selected Slack,
  reached approval denial, and executed zero action batches. Neither recorded the old provider 400
  or a local portability rejection.
- 2026-07-14: focused verification is green: `cargo fmt --all -- --check`, `git diff --check`,
  `cargo build --workspace`, `cargo test -p codewandler-flux-providers` (149 passed, one ignored live
  test), `cargo test -p codewandler-flux-flow` (131 passed), focused all-target clippy,
  `cargo test -p flux-codegate`, and the three website-sync tests. The full workspace test/clippy
  attempts reached concurrent A-82 edits only: its child adaptive-policy test initially expected an
  error instead of the intentional budget-stop outcome, then its tests held a mutex across await and
  reassigned a default field; A-81 packages remained green.

## Notes
- The original tool schema remains the safety contract. Any provider-specific projection is a wire
  compatibility view only, analogous to native operation-name aliasing.
- Design: [provider-native-schema-portability.md](../designs/provider-native-schema-portability.md).
