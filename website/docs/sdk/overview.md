---
title: SDK overview
description: "Choose the right Rust surface for conversational agents, authored flows, language tooling, and advanced flow hosts."
---

# Embedding flux in Rust

`flux-sdk` embeds the same policy-gated flow engine used by the CLI. You provide a model provider
and a workspace root; the SDK wires the Flux-Lang agent loop, built-in operations, safety envelope,
and session storage.

There is only one agent turn engine. `Client`, the CLI, sub-agents, and served agents all assemble
`flux_flow::engine::FlowEngine`, whose turn loop is an authored
[`agent-loop.flux`](../agent/agent-loop.md). `FlowClient` is the other view of the same architecture:
it exposes an individual flow's lifecycle directly instead of driving a conversation.

## Install

The SDK and its dependency closure are published on crates.io under the `codewandler-` prefix (the
short `flux-*` package names are owned by unrelated projects):

```bash
cargo add codewandler-flux-sdk codewandler-flux-providers
```

Package names are prefixed, but Rust imports stay short: `use flux_sdk::...` and
`use flux_providers::...`. The SDK is provider-neutral; concrete Anthropic, OpenAI, OpenRouter,
Ollama, Bedrock, and subscription-backed providers live in `flux-providers`.

## Choose a surface

| Need | Use | Why |
|---|---|---|
| Run conversational agent turns | `flux_sdk::Client` | The complete self-hosted Flux-Lang agent loop, session, tools, and envelope behind one `run` call. |
| Parse or run one flow explicitly | `flux_sdk::FlowClient` | Direct control over parsing, analysis, optimization, seeded inputs, and execution. This is the recommended AI-app flow API. |
| Build a flow in Rust | `flux_sdk::dsl` | Typed builder ergonomics that produce the same `DraftAst`; execute it with `FlowClient`. |
| Build language tooling or a custom host | `flux_lang` | The standalone AST, parser/formatter, analyzer, optimizer, schema, DSL, program declarations, and reference-interpreter traits. |
| Own engine/session internals | `flux_flow` | Advanced `FlowEngine`, guarded runtime adapters, durable store, replay, suspension, and voice-driver integration. |

For shell and deployment surfaces, use [`flux flow run`](../language/tooling.md) for an already
authored flow, [`flux run`](../agent/cli.md) for an agent turn, and
[`flux app run`](../agent/programs.md) for an event-driven multi-agent program.

### `Client`: conversational turns

```rust
let provider = Box::new(flux_providers::anthropic::anthropic_from_env()?);
let client = flux_sdk::Client::builder()
    .model("anthropic/opus")
    .build(provider, ".")?;

let out = client.run("Summarize the README").await?;
println!("{}", out.text); // TurnOutput: text, tool_calls, usage
```

Each `Client` turn runs the same `FlowEngine` as the CLI. Typed stages detect intent and call exact
provider-native operation schemas; `agent-loop.flux` owns evidence gathering, decisions, action-batch
approval/execution, repair, and presentation. The default client loop does not ask the model to emit
per-turn executable Flux. The explicit
[`op.register`](../agent/saved-flows.md#register-an-operation-during-a-turn) exception lets a model
propose source for exactly one composite operation; the host parses, analyzes, scopes, and guards it
before installation rather than executing it on receipt.

The builder carries `allow`/`deny` permission rules, `auto_approve(true)` for trusted headless use,
an optional system prompt and context blocks, model/token/iteration limits, explicit
`agent_loop(AgentLoopSpec)`, typed `register_op(stage_fn(...))` stages, and the OS-sandbox posture.
Because a library has no approval UI, reads are pre-allowed and other gated operations deny by
default unless you provide a broader policy.

### Let the agent ask your user

An embedded conversational client can install a schema-driven question handler. Installing it adds
and pre-allows `user.ask`; an explicit deny still wins. The runtime validates the request before
your code sees it and validates the returned value again:

```rust,ignore
use std::sync::Arc;
use async_trait::async_trait;
use flux_sdk::interaction::{
    InteractionCapabilities, InteractionResponse, UserInteraction, UserInteractionRequest,
};

struct ProductUi;

#[async_trait]
impl UserInteraction for ProductUi {
    fn capabilities(&self) -> InteractionCapabilities {
        InteractionCapabilities::text()
    }

    async fn request(
        &self,
        request: UserInteractionRequest,
    ) -> flux_core::Result<InteractionResponse> {
        // Render request.prompt and request.schema, then return reviewed JSON.
        todo!()
    }
}

let client = flux_sdk::Client::builder()
    .with_user_interaction(Arc::new(ProductUi))
    .build(provider, ".")?;
```

Yes/no, enum and unique enum-array schemas naturally map to toggles and selectors. Unsupported
shapes can use a JSON editor. Audio-capable hosts return `InteractionCapabilities::with_audio()`
and resolve `PromptAudioRef.asset_id` inside their own asset store; recordings never enter the
runtime response. This interaction contract is deliberately separate from `Approver`—a product
answer cannot approve an effect.

### `FlowClient`: one flow's lifecycle

```rust
let client = flux_sdk::FlowClient::builder()
    .model("anthropic/opus")
    .auto_approve(true)
    .build(provider, ".")?; // provider: Arc<dyn flux_provider::Provider>

let ast = client.parse(r#"flow show-doc -> String
  $doc = read("README.md")
  return $doc"#)?;
client
    .analyze(&ast)
    .map_err(|d| flux_core::Error::Other(format!("{d:?}")))?;
let out = client.execute(&ast).await?;
```

Use `run_flow` as the parse/analyze/execute convenience for stored text plus inputs. The full
registration, policy, optimization, result, and suspension contract is in the
[`FlowClient` guide](./flow-client.md).

### `dsl`: author the AST in Rust

```rust
use flux_sdk::{dsl::*, FlowClient};
use serde_json::json;

let flow = Flow::named("read_each")
    .body(|b| {
        b.each("f", lit(json!(["a.txt", "b.txt"])), |e| {
            e.collect("contents");
            e.body(|b| {
                b.call("read", [var("f")]);
            });
        });
        b.ret(var("contents"));
    })
    .build();

client
    .analyze(&flow)
    .map_err(|d| flux_core::Error::Other(format!("{d:?}")))?;
let out = client.execute(&flow).await?;
```

The DSL is a construction convenience, not a second language or a type-checker. It produces the
same `flux_lang::ast::DraftAst` as text parsing; always analyze it against the
actual operation catalog before execution.

## The lower-level libraries

Most applications should stay on `Client` or `FlowClient`. Drop below the SDK only when you need to
replace a host boundary rather than configure it:

- **`codewandler-flux-lang` / `flux_lang`** is the provider- and tool-independent language library.
  Its reference interpreter runs only through injected `OpHost`, `ValueStore`, and `FlowSink`
  traits. Use it for parsers, editors, validators, schemas, custom catalogs, or a non-flux host.
- **`codewandler-flux-flow` / `flux_flow`** adapts the language to a provider, the real tool
  registry, `Executor::dispatch`, event/value stores, and agent sinks. Use `FlowEngine` when the host
  must own cancellable turns, adaptive batch approval, cross-turn `await` sessions,
  [replay](../agent/time-machine.md), or [flow-driven voice](../agent/realtime.md).

Direct `flux-lang` interpretation does not silently inherit flux's concrete safety envelope: the
embedder supplies the host traits. `FlowClient`, `Client`, and `FlowEngine` already wire operation
calls through the envelope and are therefore the safer starting points for effectful applications.

## Sub-agents

`flux_sdk::subagents` re-exports the whole delegation bundle, so a host wires named child agents
without depending on flux's orchestration crate directly. Attach it with
`ClientBuilder::with_sub_agents` (conversational) or `FlowClient::with_sub_agents` (flow); either
way the `task` tool joins the client's catalog and a plan that calls `task(role, …)` delegates to
that role's child agent through the **same** authorization → approval → guarded-IO envelope. The
child's usage is folded into the parent turn's recorded spend.

```rust
use std::sync::Arc;

use flux_sdk::subagents::{ProviderFactory, Role, RoleRegistry, SpawnLimits, SubAgents};
use flux_sdk::tools::ToolRegistry;
use flux_sdk::{Client, Provider};

// Roles can be registered in memory (the multi-tenant path) or loaded from `.flux/agents/*.md`
// with `RoleRegistry::try_load` / `try_load_project`.
let roles = RoleRegistry::from_roles([Role {
    name: "scout".into(),
    description: "read-only reconnaissance".into(),
    model: None,             // inherit the spawner's default model
    thinking: None,
    effort: None,
    agent_loop: None,        // `adaptive`, or inline Flux-Lang for this role's outer loop
    tools: Some(Vec::new()), // a leaf: `None` inherits every parent tool, `Some([])` grants none
    prompt: "You are a scout. Investigate and report findings tersely.".into(),
}]);

// A fresh provider per child — children cannot share one `Box<dyn Provider>`.
let factory: ProviderFactory = Arc::new(|| Ok(Box::new(build_provider()) as Box<dyn Provider>));

let sub_agents = SubAgents::new(roles, ToolRegistry::new(), factory, "anthropic/opus", 4096)
    .with_limits(SpawnLimits {
        max_iterations: 30,
        max_tokens: 4096,
        wall_clock: Some(std::time::Duration::from_secs(120)),
    });

let client = Client::builder()
    .model("anthropic/opus")
    .with_sub_agents(sub_agents)
    .build(provider, ".")?;
```

`SubAgents::new(roles, child_base, provider_factory, default_model, max_tokens)` takes the child
tool surface **explicitly** rather than reusing the parent's assembled registry, so a child's
reachable ops are auditable and independent of parent registration order; each role's `tools`
allowlist subsets it. `with_authorization`, `with_approver`, `with_audit`, and `with_reasoning`
cover the remaining knobs, and `with_max_depth` (default `1`, children are leaves) bounds nesting.
`with_sub_agents` applies a 10-minute `wall_clock` when the bundle sets none, so a hung child cannot
run forever; `with_sub_agents_policy` is the sibling that also pins an explicit `AdaptiveLoopPolicy`
for every child. Roles authored as markdown parse through `try_parse_role`, which rejects malformed
frontmatter rather than defaulting it — a missing `tools` key means "inherit the parent's tools" and
is therefore security-relevant.

```sh
cargo run -p codewandler-flux-sdk --example sub_agent
```

## What else `flux_sdk` re-exports

The SDK is the single dependency an embedder needs: each module below re-exports a contract from an
internal crate so consumer code names only `flux_sdk::…`.

| Module | Contract | Guide |
|---|---|---|
| `tools` | `Tool`, `tool_fn`, `ToolSpec`/`Risk`, `ToolContext`, `ToolResult`, `ToolRegistry` — custom operations. | [`FlowClient`](./flow-client.md) |
| `approval` | `Approver`, `ApprovalChoice`, `RiskApprover`, `IntentSet` — your approval policy. | [Safety and approvals](../agent/safety.md) |
| `interaction` | `UserInteraction`, request/response and capability types — schema-driven questions rendered by your host. | [above](#let-the-agent-ask-your-user) |
| `authorization` | `AuthorizationPolicy`, `Caller`, `Trust`, `ExecutionAuthorization`, `IdentityCell` — the policy floor and resolved identity. | [Safety and approvals](../agent/safety.md) |
| `observe` | `Message`, `TurnSummary`, `RunEvent`, `ModelCost`, `EfficiencySummary`, `RunDiff`/`DiffRow`, the `EventStore`/`FlowStore` handles, and the evidence-gating types (`ToolGroup`, `SignalMatch`, `Observation`, `KIND_SIGNAL`). | [Sessions](./sessions.md#reading-a-session-back) |
| `subagents` | `SubAgents`, `SpawnLimits`, `ProviderFactory`, `Role`, `RoleRegistry`, `try_parse_role`. | above, and [Skills and roles](../agent/skills-and-roles.md) |
| `whatif` | `Counterfactual`, `WhatIf`, `WhatIfSpec`, `SweepReport`, `OffTape`, `Divergence` — world-pinned counterfactuals. | [Agent Lab](./agent-lab.md) |
| `datasource` | `LiveDatasource`, `LiveAccess`, `LiveSchema`/`LiveEntity`, typed filters, `Page`/`PageRequest`, `Row`/`Reference`. | [Datasources](./datasources.md) |
| `voice` | `VoiceSink`, `VoiceReply`, `RealtimeProvider`, `RealtimeConfig`. | [Realtime voice](../agent/realtime.md) |
| `recipes` | Parameterized DSL flow builders. | below |

Four modules are feature-gated and off by default, so their dependency closures stay out of a
default build: `test` (`test-kit`), `providers` (`providers`), `plugins` (`plugins`), and `pricing`
(`pricing`).

## Zero-key testing

`Provider` is a small trait, so tests and examples can drive the real engine and envelope with a
hermetic provider and no API key. The repository ships runnable examples:

```sh
cargo run -p codewandler-flux-sdk --example client_basic
cargo run -p codewandler-flux-sdk --example parameterized_flow
cargo run -p codewandler-flux-sdk --example dsl_loops
```

`client_basic` exercises the adaptive authored loop; `parameterized_flow` parses and executes authored
text; `dsl_loops` executes a Rust-authored `each` loop against a temporary workspace.

## Recipes

`flux_sdk::recipes` is a cookbook of parameterized DSL builders that can be analyzed and executed
by any `FlowClient`:

| Module | Recipes |
|---|---|
| `recipes::routing` | `route_intent` |
| `recipes::lookup` | `answer_with_fallback` |
| `recipes::batch` | `map_each`, `repeat_until`, `poll_for`, `race_first` |
| `recipes::resilience` | `retry_with_backoff`, `with_timeout`, `with_budget`, `try_catch` |
| `recipes::fanout` | `parallel_all` |
| `recipes::dispatch` | `match_value` |
| `recipes::compose` | `resilient_call` |

The CLI exposes the same cookbook through `flux preset list`, `flux preset help`, and
`flux preset ... --run`.

## Related docs

- [`FlowClient`](./flow-client.md) — the direct lifecycle and extension surface.
- [The agent loop](../agent/agent-loop.md) — how `Client` and the CLI detect intent, explore, approve, execute, and repair.
- [Flux-Lang overview](../language/overview.md) — the language all of these surfaces share.
- [Safety and approvals](../agent/safety.md) — the envelope embedded applications inherit.
- [Multi-agent programs](../agent/programs.md) — event-triggered applications whose journeys are flows.
