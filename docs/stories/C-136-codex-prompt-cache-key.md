---
id: C-136
title: Codex — send a cache-routing key so Responses prefix caching actually hits
pillar: Core
status: ready
priority: 7
epic: llm-cache-review
design: docs/designs/llm-cache-review.md
note: "build_responses_body (openai.rs:829) sends store:false and re-sends the full input array each round, so automatic prefix caching is the only mechanism available — and we send nothing to route successive requests of one session to the same cache shard"
---

# Codex — send a cache-routing key so Responses prefix caching actually hits

## Goal
Give the OpenAI Responses backend the affinity signal it needs to serve a session's rounds from one
prefix cache, instead of relying on load-balancer luck. Serves Core: the codex subscription path is
the second-highest-traffic provider and currently gets no deliberate cache help at all.

## Acceptance
- [ ] **Step one is a documentation check, not code.** Confirm against current OpenAI Responses
      documentation: the exact parameter name and semantics for prompt-cache routing, the minimum
      cacheable prefix, and whether it is accepted on the ChatGPT/codex backend (`CODEX_ENDPOINT`)
      as well as the API-key `openai` path. Record the findings and their source in Progress. If the
      parameter does not exist or is rejected on the ChatGPT backend, close the story as
      *no-change-needed* with the evidence — that is a valid outcome.
- [ ] Assuming it is supported: the key is derived from `RequestTrace.session_id`
      (`crates/flux-provider/src/lib.rs:83`), which every engine-issued request already carries, so
      successive rounds of one session share it and distinct sessions do not.
- [ ] The key is **not** derived from anything that would leak content or identity onto the wire —
      hash if `session_id` is ever user-derived. `RequestTrace` is documented as host-owned and
      "never serialized onto the vendor wire" (`flux-provider/src/lib.rs:81-82`); this story creates
      a deliberate, narrow exception for a derived opaque key and must say so in a comment at the
      site, or derive the key without reusing the trace type.
- [ ] Failing-first test on the built body in `crates/flux-providers/src/openai.rs`: two requests
      carrying the same `RequestTrace.session_id` produce the same key; different sessions produce
      different keys; a request with no trace omits the field entirely.
- [ ] Applied on the codex path; the API-key `openai` path follows the same rule unless the doc check
      says otherwise.
- [ ] Live-validated with the C-133 harness against `codex/*`: `cached_tokens` on rounds 2+ of the
      fixed multi-round turn improves against the recorded baseline. Before/after in the design doc.
- [ ] Standard gate green (build, test, clippy `-D warnings`, fmt, `flux-codegate`).

## Progress
- (not started)

## Notes
- The `store: false` posture is deliberate and stays: no conversation state is retained
  server-side, which is why `previous_response_id` is not an option here (design doc,
  *Alternatives considered*).
- Usage parsing already handles the read side — `cache_read_input_tokens: cached` at
  `crates/flux-providers/src/openai.rs:367,1064`, with `cache_creation` correctly left at 0 (OpenAI
  does not bill a write tier). No telemetry work needed in this story beyond C-133.
- The codex body deliberately omits `max_output_tokens` and sampling params (the ChatGPT backend
  rejects them). Anything added here must be gated the same way — check it does not 400 on the
  ChatGPT backend before shipping.
