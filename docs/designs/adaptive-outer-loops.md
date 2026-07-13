# Flux-authored adaptive outer loops

**Status:** shipped · **Story:** [A-73](../stories/A-73-flux-authored-adaptive-outer-loops.md)
· **Pillar:** Agent

## Decision

An agent turn is a Flux-Lang outer loop over typed operations. Models are cognition front-ends: they
declare intent, gather evidence with exact provider-native schemas, ask questions, propose literal
operation calls, and present results. Models do not emit Flux ASTs. Flux-Lang remains the runtime for
authored control flow, deterministic lowering, approval, guarded execution, suspension, audit, replay,
and composition with deterministic journeys.

The built-in adaptive loop is the CLI default and the one loop used by SDK `Client`, sub-agent,
server/A2A, app-agent, and other conversational text surfaces. Other loops are explicit presets or
authored `agent_loop` declarations. Flow-driven voice already runs an explicit authored flow and
shares durable `await`; the lower-level model-driven realtime mode remains an explicitly selected,
provider-owned loop whose narrower guarantee is guarded tool dispatch.

## Typed stages and capability signals

A stage is an ordinary operation with its own input and output schema. There is no universal stage
envelope. The standard library supplies interoperable artifacts (`IntentSet`, `DecisionRequest`,
`ActionBatch`, `ApprovalReceipt`, `ExecutionReport`), but custom stages may return any registered
named type and the authored loop explicitly adapts it.

Intent and subsequent stages may emit semantic signals. `capabilities.resolve` maps accumulated
signals to operation groups, then intersects the result with the live registry, agent tools,
permissions, and active `with_tools` scopes. A signal never grants authority and never invokes an
operation. An authored loop may call a deterministic mandatory operation explicitly.

Capability visibility is monotonic for one adaptive turn. Host evidence creates the initial surface;
each accepted `turn.intent` family is accumulated in the durable stage state and remains available to
later gather, action, repair, presentation, and `await`-resume phases. Every native round re-expands
that state from the live registry and re-applies the agent-tool, bare-deny, active `with_tools`, and
any authored model-stage tool ceiling. It does **not** intersect an accumulated semantic signal back down to the immutable
turn-start surface, and it does not carry a model-inferred family into an unrelated later user turn.
Accepted expansions are audited as `turn.capability_signal` observations.

Each operation declares a staging disposition: `Infer`, `Gather`, or `Capture`. `Infer` uses the
existing conservative risk/effect/intent test. `Gather` is valid only for low-risk,
side-effect-free, non-mutating calls; invalid declarations fail closed to `Capture`. Idempotency
controls repeat/cache semantics, not whether a read needs action approval: a fresh clock or status
read remains gather-safe. Opaque delegators are
`Capture` unless their real transitive contract proves otherwise.

## Default loop

The built-in Flux program performs:

```text
detect_intent
  -> resolve capabilities
  -> explore (repeat while new evidence/signals are useful)
       -> decision? present + await + resume
       -> direct answer? present
       -> actions? freeze ActionBatch
  -> approve_batch
  -> execute_batch
       -> failure? return report to the same native ledger and repair only failed work
  -> present_results
```

All loops and model rounds are bounded and cancellable. Gather outputs retain redacted provenance.
Questions park on the existing Flux suspension store, so resumption does not require a model to
reconstruct evidence.

## Action batches and approval

Provider-native effect calls are validated against the exact live operation schema and captured as
ordered `{op,input}` values. The host may lower them to literal Flux calls internally for the existing
resumable ledger and cassette, but that representation is never model output.

`approve_batch` renders the batch and aggregate risk, then returns an opaque receipt bound to the
batch fingerprint, session/turn, caller identity, and current policy context. A receipt is one-shot
and process-local; suspension or restart requires reapproval. `execute_batch` fails closed without a
matching receipt, rechecks authorization, and dispatches every call through `Executor`. A partial
failure returns per-action status and never silently re-runs completed effects.

## Selection and extension

`AgentLoopSpec` selects `Builtin(Adaptive)` or a parsed Flux AST. App syntax adds a typed
`agent_loop <name>` declaration and `agent ... loop <name>` reference. CLI/config selection is
explicit; the old magic `.flux/agent-loop.flux` override is removed.

SDK `stage_fn<I,O>` derives both schemas and registers an ordinary guarded operation. Config model
stages declare prompt, arbitrary input/output JSON Schemas, model settings, and an optional gather-only
tool ceiling. Effect proposal uses the standard native `propose_actions` stage so config cognition
cannot accidentally execute writes while reasoning.

## Compatibility and removal

Delete the model compiler and its surfaces (`emit_plan`, `plan`, `flux plan`, NL `FlowClient::compile`,
emission A/B and corpus export). Keep deterministic Flux-Lang APIs and keep decoding historical plan
events. New events use intent/stage/action-batch vocabulary.

`ai_segment` is rebuilt from the same native-schema stage substrate. In ai-agent-platform, model-owned
text and voice eventually select the adaptive preset; realtime supplies transcription/speech while the
same channel-neutral Flux loop owns cognition and effects. Platform adoption is a separate post-release
change.

## Verification

The hard safety invariant is unchanged: no signal, stage, batch, custom loop, SDK helper, or voice
adapter bypasses authorization, approval, or guarded IO. Failing-first tests cover output typing,
capability ceilings, staging, receipts, suspension, provider-history validity, local repair, and legacy
planner absence. The A-71 four-model fixture remains the correctness/call-count cutover gate; an
`s_1104`-shaped semantic-operation failure proves repair without whole-plan regeneration.

The final installed-binary cutover gate passed 12/12: Codex gpt-5.5 (`s_1150`–`s_1152`) used four
provider calls per trial, Gemini 3.5 Flash (`s_1153`–`s_1155`) used four, DeepSeek V4 Flash Nitro
(`s_1156`–`s_1158`) used four to five, and GPT-5-mini (`s_1159`–`s_1161`) used four. All answers
cited the four required real paths, fabricated no path, and invoked no legacy planner operation.
The reproducible gate is `scripts/eval-adaptive-support.sh`.
