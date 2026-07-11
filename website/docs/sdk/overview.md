---
title: SDK overview
description: "Choose the right Rust surface for conversational agents, authored flows, language tooling, and advanced flow hosts."
---

# Embedding flux in Rust

`flux-sdk` embeds the same policy-gated flow engine used by the CLI. You provide a model provider
and a workspace root; the SDK wires the Flux-Lang agent loop, built-in operations, safety envelope,
and session storage.

There is only one agent turn engine. `Client`, the CLI, sub-agents, and served agents all assemble
`flux_flow::engine::FlowEngine`, whose turn loop is itself the editable
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
| Compile or run one flow explicitly | `flux_sdk::FlowClient` | Direct control over parse/compile, analysis, optimization, seeded inputs, and execution. This is the recommended AI-app flow API. |
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

`Client` does not wrap a legacy or provider-native tool loop. Each turn runs the same
`FlowEngine` as the CLI: the model emits Flux-Lang plans, the engine dispatches their operations,
and `agent-loop.flux` decides when to gather, revise, or answer.

The builder carries `allow`/`deny` permission rules, `auto_approve(true)` for trusted headless use,
an optional system prompt and context blocks, model/token/iteration limits, and the OS-sandbox
posture. Because a library has no approval UI, reads are pre-allowed and other gated operations deny
by default unless you provide a broader policy.

### `FlowClient`: one flow's lifecycle

```rust
let client = flux_sdk::FlowClient::builder()
    .model("anthropic/opus")
    .auto_approve(true)
    .build(provider, ".")?; // provider: Arc<dyn flux_provider::Provider>

let ast = client.compile("read the doc and show it", None).await?;
client
    .analyze(&ast)
    .map_err(|d| flux_core::Error::Other(format!("{d:?}")))?;
let out = client.execute(&ast).await?;
```

Use `parse` instead of `compile` for stored Flux-Lang text when no model call should occur. Use
`run` for the one-call natural-language pipeline, or `run_flow` for deterministic stored text plus
inputs. The full registration, policy, optimization, result, and suspension contract is in the
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
same `flux_lang::ast::DraftAst` as text parsing or model compilation; always analyze it against the
actual operation catalog before execution.

## The lower-level libraries

Most applications should stay on `Client` or `FlowClient`. Drop below the SDK only when you need to
replace a host boundary rather than configure it:

- **`codewandler-flux-lang` / `flux_lang`** is the provider- and tool-independent language library.
  Its reference interpreter runs only through injected `OpHost`, `ValueStore`, and `FlowSink`
  traits. Use it for parsers, editors, validators, schemas, custom catalogs, or a non-flux host.
- **`codewandler-flux-flow` / `flux_flow`** adapts the language to a provider, the real tool
  registry, `Executor::dispatch`, event/value stores, and agent sinks. Use `FlowEngine` when the host
  must own cancellable turns, reviewed-plan execution, cross-turn `await` sessions,
  [replay](../agent/time-machine.md), or [flow-driven voice](../agent/realtime.md).

Direct `flux-lang` interpretation does not silently inherit flux's concrete safety envelope: the
embedder supplies the host traits. `FlowClient`, `Client`, and `FlowEngine` already wire operation
calls through the envelope and are therefore the safer starting points for effectful applications.

## Zero-key testing

`Provider` is a small trait, so tests and examples can drive the real engine and envelope with a
hermetic provider and no API key. The repository ships runnable examples:

```sh
cargo run -p codewandler-flux-sdk --example client_basic
cargo run -p codewandler-flux-sdk --example flow_compile
cargo run -p codewandler-flux-sdk --example dsl_loops
```

`client_basic` exercises the self-hosted agent loop; `flow_compile` covers natural language to AST
to execution; `dsl_loops` executes a Rust-authored `each` loop against a temporary workspace.

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
- [The agent loop](../agent/agent-loop.md) — how `Client` and the CLI orient, gather, execute, and revise.
- [Flux-Lang overview](../language/overview.md) — the language all of these surfaces share.
- [Safety and approvals](../agent/safety.md) — the envelope embedded applications inherit.
- [Multi-agent programs](../agent/programs.md) — event-triggered applications whose journeys are flows.
