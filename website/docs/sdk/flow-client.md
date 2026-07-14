---
title: FlowClient
description: "Build, extend, analyze, optimize, and execute Flux-Lang flows through the SDK safety envelope."
---

# FlowClient

`FlowClient` is the recommended SDK surface when your application owns a flow. It exposes the
Flux-Lang lifecycle directly while reusing `flux-flow`'s analyzer, runtime adapter, value
store, operation registry, and safety envelope.

Use the conversational [`Client`](./overview.md#client-conversational-turns) when you want the full
adaptive outer loop. Use `FlowClient` when you already own the authored flow or need explicit
lifecycle control.

## Lifecycle at a glance

| Starting point | Validate | Execute | Model calls before execution |
|---|---|---|---|
| Flux-Lang text | `parse` then `analyze` | `execute`, or `run_flow` with inputs | No, unless the authored flow calls a model op |
| Rust DSL / existing `DraftAst` | `analyze` | `execute` | No, unless the AST calls a model op |
| Seeded `DraftAst` | `analyze_seeded` | `execute_with` | No, unless the AST calls a model op |
| Read-parallelized AST | `optimize` | `execute_optimized` | No, unless the AST calls a model op |

The `run_flow` convenience pipeline aborts on parse/analysis failure before execution. When invoking
the stages separately, call `analyze` (or `analyze_seeded`) yourself before
`execute`; the direct execute methods assume the supplied AST is ready. Once execution begins, every
effectful `call` still passes through `Executor::dispatch`, and parsing or seeding data never grants
a capability.

## Build the client and its policy

```rust
let client = flux_sdk::FlowClient::builder()
    .model("anthropic/opus")
    .allow("read")
    .allow("grep")
    .deny("Bash(rm:*)")
    .auto_approve(false)
    .build(provider, ".")?; // provider: Arc<dyn flux_provider::Provider>
```

The builder assembles the built-in operations and provider-backed cognition operations into one
registry, creates a guarded workspace rooted at the supplied path, and uses an in-memory flow store.
Its controls are:

- `model` selects the cognition-operation model.
- `allow` and `deny` add permission rules; deny rules take precedence.
- `auto_approve(true)` installs a headless allow-all approver. The default is deny when a call needs
  approval because a library has no prompt UI.
- `approver` installs your own per-operation `Approver` and overrides `auto_approve`.
- `with_sandbox` pins an explicit OS-sandbox posture. Without it, the builder resolves the posture
  from the `FLUX_SANDBOX*` environment settings.
- `storage` accepts `Storage::dir(...)` to persist durable-construct state (`once`/`checkpoint`)
  across processes. The default store is in memory.
- `without_prelude` starts with an empty artifact-definition map instead of the standard
  `Claim`/`Evidence`/`Ctx`/`Answer` family.

`auto_approve(true)` should be reserved for trusted, pre-authored work. It does not bypass the
dispatcher or guarded IO, but it removes the human approval stop for calls that policy permits.

## Extend the operation and type surface

A new client exposes `registry`, `op_names`, and `prelude_defs` for inspection. Mutating registration
methods return `&mut Self`, so a host can assemble its domain before analyzing flows:

- `register_op` adds one `Arc<dyn Tool>`.
- `register_op(stage_fn::<I, O, _, _, _>(...))` adds a closure-backed typed operation with
  independent derived input/output schemas; the analyzer infers `O` at its call sites.
- `register_pack` installs a group of tools into the registry.
- `with_sub_agents` registers `task` and attaches a `SubAgents` spawner. If the bundle has no
  wall-clock limit, the SDK supplies a ten-minute default. `with_sub_agents_policy` additionally
  accepts a child `AdaptiveLoopPolicy`, so intent and exploration may use independent same-provider
  models, reasoning effort, output limits and call ceilings without changing the parent agent. The
  conversational `ClientBuilder` exposes the same `with_sub_agents_policy` sibling while its existing
  `with_sub_agents` path keeps the default child policy.
- `register_composites` installs Flux-Lang `CompositeOpDecl`s so flows can call them like ordinary
  operations. `parse_module` is the deterministic loader for modules that declare these ops.
- `register_prelude` merges additional artifact `$defs` into the client's definition map for
  inspection and downstream catalog enrichment.

All registered tools still execute through the same permission, approval, redaction, and guarded-IO
path. A composite operation is a nested flow; its inner calls do not inherit authority from the
wrapper.

Child cognition policy and spawn limits are deliberately separate. `AdaptiveLoopPolicy` bounds the
native intent/explore model calls inside one logical child run. `SpawnLimits` bounds the child's
authored outer-loop iterations, fallback output-token setting, and whole-run wall clock. Existing
`with_sub_agents` callers keep the standard adaptive policy; direct spawner hosts can opt in with
`LocalSpawner::with_adaptive_policy` or
`SubAgents::into_spawner_with_adaptive_policy`.

## Parse or construct

```rust
// Authored Flux-Lang text -> DraftAst, with no provider call.
let parsed = client.parse(
    r#"flow heading
  $doc = read("README.md")
  return $doc"#,
)?;
```

`parse(text)` is total and returns an error for malformed source rather than panicking.
`parse_module(text)` also recognizes module-level flows, composite operations, and multi-agent
program declarations. Rust callers can instead construct the same `DraftAst` through
`flux_sdk::dsl`. In both cases, analyze against the client's final registry before execution.

## Analyze effects and optimize

Always analyze parsed text, existing ASTs, or DSL output against the client's final registry:

```rust
client
    .analyze(&ast)
    .map_err(|diagnostics| flux_core::Error::Other(format!("{diagnostics:?}")))?;
```

`analyze` reports unknown operations, wrong arguments/types, invalid control-flow placement, and
unbound symbols before effects begin. Flow parameters count as bound. When a host injects values
that were not declared as parameters, call `analyze_seeded(&ast, input_names)` instead.

For approval UIs and visual editors, the lower-level
`flux_lang::analyze::annotate_effects(&ast, &ops)` returns each call's effects, risk, and
idempotency keyed by its diagnostic node path. See [Types and effects](../language/types-and-effects.md#effects).

`optimize` performs analysis/lowering and returns a `PhysicalPlan` that groups independent
read-only top-level work into parallel stages while fencing unknown or effectful work.
`optimize_seeded` accepts the same prebound-name contract as `analyze_seeded`.
`execute_optimized` optimizes and runs in one call. Optimization never grants permission: every
operation in every stage still dispatches independently through the envelope.

## Execute stored flows with inputs

`execute(&ast)` uses the client's in-memory store, so symbols produced by one call remain available
to later calls on that client. `execute_with(&ast, inputs)` instead creates a fresh store for that
invocation, seeds its `$name` values, and discards that per-run state afterward. This keeps repeated
runs with different inputs isolated.

```rust
use serde_json::{json, Map};

let mut inputs = Map::new();
inputs.insert("path".into(), json!("README.md"));

client
    .analyze_seeded(&ast, inputs.keys().cloned())
    .map_err(|d| flux_core::Error::Other(format!("{d:?}")))?;
let out = client.execute_with(&ast, inputs).await?;
```

Seeding is data injection only. It cannot register an operation, widen a permission rule, or skip
approval. A flow-local binding may shadow a seed; an unseeded reference fails; unused extra inputs
are ignored.

`run_flow(source, inputs)` is the stored-flow convenience:

```text
parse -> analyze -> execute_with
```

Declare reusable inputs in the flow header so plain analysis recognizes them. The command-line
equivalent is [`flux flow run --inputs/--arg`](../language/tooling.md#flux-flow-list--run--discover-and-execute-saved-flows).

## Read the result

Every execution method returns `ExecutionResult`:

| Field or helper | Meaning |
|---|---|
| `result` | The explicit `return`, or the last node's rendered value. |
| `transcript` | The labeled model-facing views produced during execution. |
| `steps` | Number of dispatched operations. |
| `tool_calls` | Operation names in dispatch order. |
| `usage` | Summed token spend of model-backed cognition ops; `None` if none were billed. |
| `parse::<T>()` | Deserialize `result` into an application type. |
| `answer()` | Deserialize the standard prelude `Answer` artifact. |

Typed prelude artifacts such as `Answer`, `Claim`, `Evidence`, `Patch`, `TestResult`, and `Verdict`
are re-exported from `flux_sdk::flow`.

## Session, suspension, and voice boundaries

`FlowClient` is deliberately a one-flow façade, not a durable conversation host:

- Its store is in memory by default; `storage(Storage::dir(...))` persists `once`/`checkpoint` state
  across processes. Use `FlowEngine` with a durable `FlowStore` when values, run traces, and
  suspensions must survive process exit.
- `execute`, `execute_with`, and `run_flow` do not resume a top-level `await`. If execution suspends,
  they return an explicit error after reporting that the one-shot SDK path has no resume hook.
  Drive authored conversations through `FlowEngine::start_flow_turn`; later `run_turn` calls resume
  the stored suspension. See [Flow-driven sessions](../language/durability.md#flow-driven-sessions--await-as-the-conversation).
- `run_voice_session` is the model-driven realtime façade: it advertises the client's registered
  tools once and dispatches voice-model tool calls through the same executor. For a flow-driven
  call, use `VoiceSessionDriver::run_flow_turns` with `EngineVoiceHandler`, which owns a
  `FlowEngine`. See [Realtime voice](../agent/realtime.md).

For cancellable conversational turns, adaptive batch approval, hermetic replay, forks, durable
sessions, or flow-driven voice, drop to `flux_flow::engine::FlowEngine` rather than rebuilding those
seams around `FlowClient`.

## Related docs

- [SDK overview](./overview.md) — choose between `Client`, `FlowClient`, the DSL, and lower-level crates.
- [Flux-Lang execution model](../language/execution-model.md) — analyzer, optimizer, values, and dispatch semantics.
- [Saved flows and custom operations](../agent/saved-flows.md) — reusable project/global flows and composites.
- [Time Machine](../agent/time-machine.md) — replay, fork, diff, and resumable stored flows.
- [Safety and approvals](../agent/safety.md) — permission and approval behavior during dispatch.
