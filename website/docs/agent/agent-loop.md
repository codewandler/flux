---
title: The agent loop
description: "How typed intent, scoped exploration, batch approval, and authored Flux-Lang control one turn."
---

# The agent loop

<!-- Editors: the contributor deep-dive of this loop is docs/agent-loop.md in the repository. The
two texts are deliberately independent, not mirrors; keep both consistent with the shipped tree. -->

Every conversational turn is driven by an authored Flux-Lang program. The CLI, SDK `Client`, server,
sub-agents, apps, and conversational text adapters all use the same loop; Rust only supplies the provider,
session, operation registry, cancellation, and safety envelope.

Flow-driven voice selects an explicit authored flow and shares the same durable `await` and guarded
runtime. The experimental model-driven realtime SDK mode is deliberately separate: the provider
owns that loop, while every effect still crosses the executor envelope.

The important boundary is stronger than “the model is not the runtime”: **the default conversational
loop never asks the model for per-turn executable Flux**. Models interpret intent and propose literal
calls inside provider-native typed stages. The authored Flux-Lang loop owns order, bounds, decisions,
approval, execution, and stopping.

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

Structured intent and exploration results also fail closed. They must be tagged JSON objects before
the authored loop reads them. A scalar or untagged response stops the turn and names the stage and
returned type, with a bounded redacted excerpt retained in the trace, instead of surfacing as a
misleading Flux-Lang field-access error or being treated as a default intent.

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

### The explicit `op.register` seam

The model-facing `op.register` operation is a deliberate, narrower exception. An agent may propose
source containing **exactly one composite `op`**; the host parses and analyzes it against the live
catalog before installing it at `turn`, `session`, `project`, or `global` scope. Project and global
installation are guarded filesystem writes, replacement must be explicit, and every inner operation
still dispatches through the normal envelope.

This extends the agent's reusable vocabulary; it does not replace the authored outer loop or make
generated source executable on receipt. See [Saved flows and custom operations](./saved-flows.md).

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

One logical adaptive turn has a 50-call default ceiling spanning intent repair, exploration, and
every decision resume. Exceeding it fails clearly instead of returning an ungrounded answer. Use
`--max-model-calls` for a one-off override, or configure the stages:

```toml
[agent.adaptive]
max_model_calls = 50

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

The authored decision/batch repeat has a separate 50-iteration default. It counts outer control-flow
iterations, not provider calls. Override it with `--max-iterations`, `[agent] max_iterations`,
`AgentSpec::max_iterations`, or the SDK builder's `max_iterations` method:

```toml
[agent]
max_iterations = 50
```

The accepted range is 1 through 1,000. The upper bound is checked before the built-in repeat is
expanded into its durable top-level state machine.

An authored `ai_segment` owns a third, local bound: its required `max_rounds` is honored exactly and
is not clamped to either normal-turn default. All three controls must be positive.

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
max_iterations = 50
```

Apps may define and select a loop in the same source file:

```flux
agent_loop support
  intent = detect_intent()
  stage = explore(state: intent.state)
  answer = present_results(step: stage)
  return answer

agent guide
  model "openrouter/google/gemini-2.5-flash"
  loop "support"
```

Custom loops are analyzed before a turn starts. They can combine deterministic operations with
`detect_intent`, `explore`, `approve_batch`, `execute_batch`, `present_results`, `ai_segment`,
`observe`, and `await`; they cannot bypass the envelope.

## Resolved loop bindings

Before an agent can run, Flux resolves its selection into a versioned loop binding. The binding
records the logical profile and revision, runner kind, immutable source reference and SHA-256,
entry point, required operations, and required runtime features. An omitted general selector is not
left as `None`; it becomes the explicit built-in `adaptive@1` binding.

Turn-start, status, streaming, and terminal receipts expose this bounded identity and digest, never
the loop source or prompts. Flux validates the source digest, syntax, required operation catalog,
and runtime features before the first model request. A missing profile or capability mismatch is
therefore an admission error, not a silent fallback to another loop.

Bindings belong to sessions. Resume and restart reconstruct the admitted source from its
digest-addressed snapshot, so editing a selected file affects only new sessions. Passing a
different `--loop` while resuming a live session is refused; start a new session to change behavior.
Required operations and runtime features are compared as canonical sorted sets, so a receipt from
an older build is not rejected merely because insertion order changed; every other identity field
remains exact.
Roles and `task` children resolve their own declared bindings rather than inheriting the parent's
loop or conversation.

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
