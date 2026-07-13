# Adaptive-loop hardening

**Status:** implemented (A-76)  
**Date:** 2026-07-13  
**Extends:** [adaptive outer loops](adaptive-outer-loops.md)

## Problem

The adaptive loop's first proof established the right decomposition—intent signals select a small
capability family, native schemas drive exploration, and the host freezes effects into an approved
batch. The default cutover then exposed four structural gaps:

1. the shipped Flux program awaited only the first decision and terminated on a later one;
2. monotonic capability surfacing was stored on the engine, so a shared engine leaked catalog shape
   between sessions;
3. each exploration invocation had a local round bound, but a logical run spanning repairs and
   durable decision resumes had no shared call bound;
4. integration routing and model-call traces did not expose enough semantic or timing information
   to diagnose slow or missed routes without reading raw provider traffic.

These are loop/runtime contracts, not prompt-tuning problems. The fix therefore belongs in authored
control flow and typed host state. Prompt changes are not used to special-case individual tools.

## Design

### 1. One repeatable decision seam

Every `kind = decision` result is handled inside the bounded execute/revise repeat. The flow presents
the typed `DecisionRequest`, awaits `agent.decision`, then calls `explore` with the exact opaque state
and reply. Flux-Lang's existing suspension store persists the continuation and all prior bindings.
There is no second queue or pause API. A resumed exploration may return another decision and follow
the identical path.

An execution report is inserted into the provider-native ledger before another decision can be
raised. The completed batch and consumed approval receipt are never reconstructed, so resuming a
decision cannot replay successful actions.

### 2. Session-scoped monotonic surfacing

The engine keeps `session_id → active group set`, not one global set. Each session's set only grows,
preserving prompt-cache stability within that conversation, while unrelated sessions begin from
their own evidence. Advertising remains non-authoritative: the dispatcher still applies live tool
wiring, permissions, approval, and guarded IO.

### 3. Compact deterministic routing evidence

Every live integration group contributes a routing-only index entry: family name, description, and
declared `turn.intent` signals. Plugin manifests already carry these hints through `ToolGroup`;
aliases, semantic capabilities such as `chat`, and URL hosts are represented as explicit signals.
The CLI adds the plugin name and declared endpoint/HTTP hosts to legacy implicit groups. Failed,
stale, or merely installed-but-unloaded plugins never contribute a group and therefore never become
candidates.

Strong lexical/URL evidence is matched before the intent call. One match is a mandatory family hint
that the intent declaration cannot drop. More than one live family returns a typed
`DecisionRequest`; the user chooses before any integration schema is exposed. Zero matches keeps the
normal model router. The routing index changes visibility only and never grants permission.

### 4. One logical-run model-call budget

The durable adaptive state carries cumulative intent and exploration call counts. The default total
ceiling is 12, checked before every provider request and serialized across `await`/resume and process
restart. Exhaustion returns a precise error naming used/allowed calls; it never silently falls back
to an ungrounded answer. `AgentSpec`, config, and `--max-model-calls` can lower or raise the bound.

`AgentStagePolicy` optionally overrides model, effort, output-token cap, and stage-call cap for the
intent or exploration phase. Missing values inherit the agent. A model override must resolve on the
already-selected provider; a different provider is rejected during assembly/startup. No second
provider or credential path is introduced.

### 5. Correlated telemetry, not hidden reasoning

Each provider request is correlated with session, turn, stage, and native round. A durable,
redacted `model.call` observation records wall duration, TTFT, input/cache/output/reasoning usage,
message/tool/schema byte counts, and repair attempt. Approval wait and execution duration use
separate observations. `--show-loop` renders one compact line per stage call. The existing
`FLUX_MODEL_TRACE=full` remains the explicit sensitive opt-in for request bodies; summaries never
claim to expose private chain-of-thought.

### 6. Verification

Unit/integration tests use scripted providers and a fake integration family so routing, suspension,
approval, partial failure, call exhaustion, and shared-engine isolation are deterministic in CI.
The live adaptive matrix remains a pre-release quality gate: three trials per supported model,
scored on outcome and call/latency bounds. A faster intent-stage model becomes a default only after
that matrix shows parity; this story adds the policy seam but does not guess a new default.

## Compatibility and release shape

The behavioral correctness, isolation, bounds, telemetry, and docs are additive/fix-level. Public
`AgentSpec` stage-policy fields are additive at runtime but source-breaking for exhaustive Rust
struct literals under pre-1.0 semantics; the eventual release that exposes them must therefore be a
minor bump. No release is cut as part of implementation unless explicitly requested.
