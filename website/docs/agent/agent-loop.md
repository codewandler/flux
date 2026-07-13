---
title: The agent loop
description: "How typed intent, scoped exploration, batch approval, and authored Flux-Lang control one turn."
---

# The agent loop

Every conversational turn is driven by an authored Flux-Lang program. The CLI, SDK `Client`, server,
sub-agents, apps, and conversational text adapters all use the same loop; Rust only supplies the provider,
session, operation registry, cancellation, and safety envelope.

Flow-driven voice selects an explicit authored flow and shares the same durable `await` and guarded
runtime. The experimental model-driven realtime SDK mode is deliberately separate: the provider
owns that loop, while every effect still crosses the executor envelope.

The important boundary is stronger than “the model is not the runtime”: **the model does not generate
Flux code**. Models participate inside typed stages and make provider-native operation calls. The
authored loop owns order, bounds, decisions, approval, execution, and stopping.

## The default adaptive loop

```text
detect intent
  → intersect signals with registered + wired + permitted operations
  → explore with those operations' exact native schemas
      → gather-safe reads execute through the envelope and become evidence
      → questions park on await and resume the same state
      → effectful calls are captured, not executed
  → freeze captured calls into an immutable action batch
  → approve the batch and issue a one-shot receipt
  → execute each action through authorization → approval scope → guarded IO
      → return failures to the same native ledger for local correction
  → present results
```

Intent and capability signals control visibility, not authority. A signal can surface only a live
operation inside the agent's tool, permission, policy, and app capability ceilings. Every selected
operation still passes through the executor.

Integration manifests contribute a compact routing index of declared aliases, semantic capabilities
such as `chat`, and URL hosts. One exact live match is mandatory evidence the intent model cannot
drop. Several matches ask you to choose before any integration schema is exposed. Only loaded,
wired integrations enter the index, and surfaced capabilities stay stable within one session without
leaking into another session on a shared engine.

Operation metadata—not prompt wording—decides staging:

- Low-risk, side-effect-free reads may gather evidence immediately, including fresh reads such as
  the current clock whose values must not be cached.
- Mutating, destructive, or opaque calls are captured as literal `{op, input}` data.

The host validates every input against the exact registered schema and constructs the ordered batch.
The model cannot label a write as a read or mint an approval receipt. Idempotency controls reuse;
effects, risk, concrete intents, and staging policy control whether a call may gather.

## Why not generate one Flux plan?

A one-shot generated graph asked a model to choose operations, reproduce their argument schemas in a
second language, and invent all control flow before it had evidence. A small argument mistake could
force whole-plan regeneration. That path has been removed.

Flux now places determinism at the useful seams:

| Concern | Owner |
|---|---|
| intent, semantic choice, answer wording | typed model stage |
| operation schemas and visibility | live host registry |
| order, bounds, branching, await/resume | authored Flux-Lang |
| action-batch identity and receipt | host runtime |
| effects | authorization → approval → guarded IO |
| replay and audit | event and flow stores |

## Questions and local repair

Exploration may return a typed decision request. The loop renders the question and suspends at an
ordinary Flux-Lang `await`. The flow store retains every prior binding, including the opaque native
conversation ledger. The next user message resumes that exact state instead of asking a model to
reconstruct it. Decisions are repeatable: ambiguity during routing, a question before execution, and
a question discovered after an execution report all park on the same durable `agent.decision` await.
Resuming cannot reconstruct or replay a batch whose one-shot receipt was already consumed.

After approval, a receipt is valid for one exact batch, session, caller/authority context, and policy
context, and can be consumed only once. Changed, stale, reused, denied, or cross-session receipts fail
closed. If an action fails, later actions are skipped and the execution report returns to the same
native ledger. Completed effects are not silently replayed; the model can correct only the failed work
and propose a new batch.

## Watch it work

The CLI surfaces the initial wait and capability result even when machinery is hidden:

```text
intent…
◆ intent: update the release notes
  capabilities: workspace.read, workspace.write
exploring…
```

Reveal the authored machinery or its structural Flux nodes when diagnosing behavior:

```bash
flux run --show-loop "update CHANGELOG.md after checking the current version"
flux run --trace-loop "update CHANGELOG.md after checking the current version"
```

`--show-loop` also prints one compact line per model call with stage, round, wall time, TTFT,
operation count, and schema size. Redacted `model.call` evidence retains session/turn correlation;
approval outcomes include wait time and executed batches include duration. `FLUX_MODEL_TRACE=1`
adds provider transport milestones. Exact request bodies appear only with the explicitly sensitive
`FLUX_MODEL_TRACE=full` setting. `/evidence` shows the durable trail.

## Bound or tune the built-in stages

One logical adaptive turn has a 12-call default ceiling spanning intent repair, exploration, and
every decision resume. Exceeding it fails clearly instead of returning an ungrounded answer. Use
`--max-model-calls` for a one-off override, or configure the stages:

```toml
[agent.adaptive]
max_model_calls = 10

[agent.adaptive.intent]
model = "codex/gpt-5.5"
effort = "low"
max_tokens = 1024
max_calls = 2

[agent.adaptive.explore]
effort = "high"
max_tokens = 8192
max_calls = 8
```

Missing values inherit the agent. Stage models must use the agent's existing provider; a matching
provider prefix is stripped, while a cross-provider override fails before any request. SDK callers
set the same policy through `AgentSpec` or the client builder.

## Select or author a loop

The adaptive preset is the default. Selection is explicit:

```bash
flux run --loop adaptive "summarize this repository"
flux run --loop loops/support.flux "triage this request"

flux loop show
flux loop eject
flux run --loop .flux/agent-loop.flux "use my edited loop"
```

`flux loop eject` only copies the preset. Merely creating `.flux/agent-loop.flux` does not change
runtime behavior. Project config can select it explicitly:

```toml
[agent]
loop = "loops/support.flux"
```

Apps may define and select a loop in the same source file:

```flux
agent_loop support
  $intent = detect_intent()
  $step = explore({state: $intent.state})
  $answer = present_results({step: $step})
  return $answer

agent guide
  model "openrouter/google/gemini-2.5-flash"
  loop "support"
```

Custom loops are analyzed before a turn starts. They can combine deterministic operations with
`detect_intent`, `explore`, `approve_batch`, `execute_batch`, `present_results`, `ai_segment`,
`observe`, and `await`; they cannot bypass the envelope.

## Flow versus journey

A **flow** is a reusable deterministic computation: typed inputs, explicit operations and control
flow, and a result. A **journey** is the application lifecycle around one or more flows: triggers,
channels, conversations, decisions, waits, and delivery. A journey can call flows; an agent loop is a
specialized flow that drives a conversational turn.

For example, a documentation-assistant journey can structurally require “search the handbook before
every answer,” while retrieval and answer construction remain reusable flows and typed model stages.

## Related docs

- [Tutorial](../tutorial.md) — watch the adaptive loop, then move a reliability requirement into a journey.
- [Safety and approvals](./safety.md) — the envelope every operation crosses.
- [Durability](../language/durability.md) — `await`, resume, and flow-driven sessions.
- [CLI](./cli.md) — loop selection and diagnostics.
