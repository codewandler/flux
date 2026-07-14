# Adaptive budget coherence

**Status:** implemented (A-77)  
**Date:** 2026-07-14  
**Extends:** [adaptive-loop hardening](adaptive-loop-hardening.md)

## Problem

Three independent counters currently bound one adaptive turn. `max_model_calls` is public and
durable but defaults to 12; `adaptive_explore` has another hard 12-round clamp; and the authored
decision/batch repeat defaults to 25. The duplicate native clamp makes
`--max-model-calls 50` ineffective. It also silently reduces an authored
`ai_segment({max_rounds: 50})` to 12.

## Decision

Normal adaptive turns have one provider-call budget. Its default is 50 and the existing
`[agent.adaptive] max_model_calls` / `--max-model-calls` / `AdaptiveLoopPolicy` surface owns it.
Intent repairs, exploration, execution repair, and every durable decision resume consume the same
counter. The exploration loop derives its remaining rounds from that state; no second native-round
constant exists. A per-stage `max_calls` remains a narrowing ceiling.

`ai_segment` is a separately authored bounded cognition node. Its required `max_rounds` value is its
provider-call budget and is no longer clamped by the normal-turn default. Provider usage still folds
into the enclosing token/cost accounting and every operation stays inside the segment's exact tool
ceiling and the shared safety envelope.

The outer Flux repeat counts decision/batch state-machine iterations, not provider calls. It also
defaults to 50 but remains a separate setting: `[agent] max_iterations`, CLI
`--max-iterations`, `AgentSpec.max_iterations`, and the SDK builder. Precedence is CLI, project
config, user config, then the default. Values must be in `1..=1_000`; the shared engine loader
checks that practical bound before the built-in repeat is expanded into a durable top-level state
machine. A sub-agent spawner may intentionally narrow this through its separate `SpawnLimits`; that
explicit child resource ceiling is not a competing hidden default.

## Verification

Scripted providers pin the exact 50/51 boundary, a non-completing `ai_segment` pins its authored
bound, and config/CLI tests pin precedence and early validation. Assembly tests accept 1,000 and
reject 1,001 before repeat lowering. Suspension tests prove counters survive resume. None of these
changes touches capability visibility, authorization, approval, dispatch, or guarded IO.
