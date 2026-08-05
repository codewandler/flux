# The agent loop

> Contributor copy. The user-facing treatment of this loop is
> [`website/docs/agent/agent-loop.md`](../website/docs/agent/agent-loop.md) — deliberately
> independent texts over the same behavior, not mirrors; keep both consistent with the shipped
> tree.

flux's turn loop is an authored Flux-Lang program. `FlowEngine::run_turn_cancellable` only supplies
the session, cancellation token, provider, operation registry, and safety envelope; the program in
[`crates/flux-flow/assets/agent-loop.flux`](../crates/flux-flow/assets/agent-loop.flux) owns the
sequence. The same loop runs in the CLI, SDK `Client`, server/A2A, sub-agents, and app agents.
Flow-driven voice selects an explicit authored flow over the same durable suspension/runtime seams;
the experimental model-driven realtime mode is a separate, explicitly selected provider-owned loop.

The key boundary is deliberate: **the model does not generate Flux code**. Flux owns the reliable
control flow; a model participates only inside typed stages and through provider-native operation
calls.

## The default adaptive loop

The built-in loop performs this bounded sequence:

```text
detect_intent
  -> intersect signals with registered + wired + permitted operations
  -> explore using those operations' exact native schemas
       -> safe reads execute through the envelope and return evidence
       -> a question parks on await and resumes the same state
       -> effectful calls are captured, not executed
  -> freeze captured calls into an immutable ActionBatch
  -> approve_batch -> one-shot receipt bound to batch/session/caller/policy
  -> execute_batch -> authorization -> approval scope -> guarded IO
       -> failures return to the same native model ledger for local correction
  -> present_results
```

Intent and capability signals control visibility, not authority. A signal can surface only an
operation that is present in the live registry and inside the agent's tool, permission, policy, and
`with_tools` ceilings. Every selected operation still traverses `Executor::dispatch`.

Integration manifests contribute a compact routing index of declared aliases, semantic capabilities
such as `chat`, and URL hosts. One exact live match is mandatory routing evidence the intent model
cannot discard; multiple matches produce a decision before any integration schema is exposed. Only
successfully loaded and wired integrations enter the index. Surfacing is monotonic within a session
for prompt stability and isolated between sessions on a shared engine.

Exploration distinguishes two kinds of native calls:

- A low-risk, side-effect-free read may run immediately so its actual redacted result becomes
  evidence, including fresh reads such as `now` whose result must not be cached.
- A mutating, destructive, or opaque call is captured as literal `{op, input}` data.
  The host validates it against the live schema and later freezes it into an ordered batch.

The model cannot mark a write as a read. Effects, risk, concrete intents, and staging disposition
decide whether a call may gather; idempotency decides whether a result may be reused. These are
host-owned contracts.

The structured stage boundary is fail-closed too. `detect_intent` and `explore` must return a JSON
object with a string `kind` before their result reaches the authored loop. A scalar or untagged
object stops the turn instead of being treated as a default intent: the error names the producing
stage and value type, includes a bounded redacted excerpt of what it returned, and is retained in
the replayable step trace. This validation belongs to the host adapter rather than Flux-Lang, whose
strict field access continues to catch ordinary authored-flow mistakes.

## Why this replaced model-generated plans

A one-shot model-generated graph asked the least reliable component to choose operations, reproduce
their argument schemas inside a second language, and invent all control flow before it had evidence.
Failures often required regenerating the whole graph. The adaptive loop keeps the useful model work
— interpreting intent, selecting among visible capabilities, and reasoning over evidence — while
placing determinism at the seams where mistakes have consequences.

The resulting division is:

| Concern | Owner |
|---|---|
| intent, semantic choice, answer wording | typed model stage |
| operation schemas and visibility | live host registry |
| flow order, bounds, branching, await/resume | authored Flux-Lang |
| batch identity and approval receipt | host runtime |
| effects | authorization → approval → guarded IO |
| replay/audit | event and flow stores |

## Decisions and durable resume

An exploration stage may return a typed decision request. The outer loop presents the question and
parks on Flux-Lang's ordinary `await`. The flow store retains the prior bindings, including the
opaque native-stage ledger. The user's next message resumes that exact flow; it does not ask a model
to reconstruct the earlier evidence or plan. This seam is repeatable: routing ambiguity, a question
before execution, and a question discovered after an execution report all use the same durable
`agent.decision` await. A resume never reconstructs or re-executes an already consumed batch.

If execution returns a partial failure, completed actions remain recorded and the report goes back
to the same exploration ledger. The model may correct only failed work and propose a new batch.
Receipts are one-shot: changed, stale, reused, denied, cross-session, or authority-mismatched batches
are rejected.

## Observe it

The initial provider wait is visible in the CLI:

```text
intent…
◆ intent: update the release notes
  capabilities: workspace.read, workspace.write
exploring…
```

Use `--show-loop` to include the authored machinery operations, and `--trace-loop` for the structural
Flux nodes:

```bash
flux run --show-loop "update CHANGELOG.md after checking the current version"
flux run --trace-loop "update CHANGELOG.md after checking the current version"
```

`--show-loop` also prints one compact line per model call with stage, round, wall time, TTFT,
operation count, and schema size. The same redacted data is stored as `model.call` evidence with
session/turn correlation; approval outcomes carry wait time and executed batches carry duration.
`FLUX_MODEL_TRACE=1` adds provider-transport milestones, while only the explicit
`FLUX_MODEL_TRACE=full` mode includes sensitive request bodies. `/evidence` shows the shared audit
observations for the current session.

## Bound or tune the built-in stages

One logical adaptive turn has a 50-call default ceiling across intent repair, exploration, and every
decision resume. Exhaustion fails honestly instead of producing an ungrounded answer. Override the
total from the CLI with `--max-model-calls`, or set independent same-provider stage policy in config:

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

Missing stage values inherit the agent. A provider prefix must match the agent's provider and is
stripped before the request; a cross-provider override fails at startup. Embedded callers use
`AgentSpec::adaptive_policy` or the SDK builder's `adaptive_policy` method.

The authored decision/batch repeat has a separate 50-iteration default. It counts outer control-flow
iterations, not provider calls. Override it with `--max-iterations`, `[agent] max_iterations`,
`AgentSpec::max_iterations`, or the SDK builder's `max_iterations` method:

```toml
[agent]
max_iterations = 50
```

The accepted range is 1 through 1,000. The upper bound is checked before the built-in repeat is
expanded into its durable top-level state machine.

An authored `ai_segment` owns a third, local bound: its required `max_rounds` value is honored as
written and is not reduced to either normal-turn default. All three controls must be positive.

## Select or author a loop

Adaptive is the default. Selection is always explicit; merely creating `.flux/agent-loop.flux` does
not change runtime behavior.

```bash
flux run --loop adaptive "summarize this repository"
flux run --loop loops/support.flux "triage this request"

flux loop show                 # print the built-in preset
flux loop eject                # copy it to .flux/agent-loop.flux for editing
flux run --loop .flux/agent-loop.flux "use my loop"
```

Project config uses the same selector:

```toml
[agent]
loop = "loops/support.flux"
max_iterations = 50
```

Roles may carry `agent_loop` source in frontmatter. `AgentSpec::agent_loop` and the SDK builder use
`AgentLoopSpec`. A Flux app can declare and select a loop in the same source file:

```flux
agent_loop support
  $intent = detect_intent()
  $step = explore({ state: $intent.state })
  $answer = present_results({ step: $step })
  return $answer

agent guide
  model "openrouter/google/gemini-2.5-flash"
  loop "support"
```

Custom loops are ordinary validated Flux-Lang. They may combine deterministic operations,
`detect_intent`, `explore`, `approve_batch`, `execute_batch`, `present_results`, `ai_segment`,
`observe`, and `await`. Unknown operations fail validation before a turn starts.

## Flow versus journey

A **flow** is a reusable deterministic computation: typed inputs, explicit operations and control
flow, and a result. A **journey** is the application-level interaction lifecycle around one or more
flows: triggers, channels, conversations, decisions, waits, and delivery. A journey can call flows;
an agent loop is a specialized flow that drives one conversational turn.

That distinction is useful, not restrictive. A docs assistant journey can own the invariant “search
the handbook before every answer,” while its retrieval and answer-building steps remain reusable
flows and typed model stages.

See [architecture.md](architecture.md#agent-loop-sessions-context) and the
[engine operation reference](../crates/flux-flow/docs/ops-reference.md).
