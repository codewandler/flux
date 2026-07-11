# flux-sdk

The high-level library API for [flux](https://github.com/codewandler/flux) — embed a tool-enabled,
policy-gated agent in your own Rust program. You supply a `Provider` (a model backend) and a workspace
root; the SDK wires the agent loop, the built-in tools, the safety envelope, and a session.

The guiding idea is **"the LLM is not the runtime"**: the model emits a Flux-Lang plan (an execution
graph), and a deterministic engine runs it through a non-bypassable safety envelope.
There is one turn engine: `Client`, the CLI, sub-agents, and served agents all assemble
`flux_flow::engine::FlowEngine`, whose loop is itself a Flux-Lang program.

## Install

```sh
cargo add codewandler-flux-sdk codewandler-flux-providers
```

The closure is published under a `codewandler-` prefix (the bare `flux-*` names are taken on crates.io),
but the **import paths are unprefixed** — the crate `codewandler-flux-sdk` is `use flux_sdk::…` and
`codewandler-flux-providers` is `use flux_providers::…`. `flux-providers` supplies the concrete
`anthropic`/`openai`/`openrouter`/`ollama` backends; the SDK itself is provider-agnostic.

## Three front doors

| Surface | What it is | Example |
|---|---|---|
| [`Client`] | A conversational turn through the self-hosted Flux-Lang agent loop and safety envelope. | `examples/client_basic.rs` |
| [`FlowClient`] | The direct Flux-Lang lifecycle: parse/compile, analyze, optimize, seed, and execute. | `examples/flow_compile.rs` |
| [`dsl`] | Author the AST **in Rust** — builder primitives (loops + control-flow) that compile to the Flux-Lang AST, then run via `FlowClient`. | `examples/dsl_loops.rs` |

All three examples are hermetic (a mock provider) and run with no API key:

```sh
cargo run -p codewandler-flux-sdk --example dsl_loops      # build loops/control-flow with the DSL, execute them
cargo run -p codewandler-flux-sdk --example client_basic   # the self-hosted Flux-Lang agent loop
cargo run -p codewandler-flux-sdk --example flow_compile   # NL → AST → execute
```

Two **domain** examples show the DSL on real tasks, with the model/datasource adapters mocked (registered
stub ops) so they run with no API key:

```sh
cargo run -p codewandler-flux-sdk --example intent_routing # classify an utterance, then `route` to a handler
cargo run -p codewandler-flux-sdk --example faq_lookup     # KB lookup + `fallback` escalation → a typed `Answer`
```

## Quick start — the Rust DSL

Build a flow with native Rust, then analyze + execute it through the real envelope. Loops
(`each`/`repeat`/`loop_for`/`race`) and the control-flow guards (`match`/`route`/`fallback`/
`timeout`/`budget`) are first-class.

```rust,ignore
use std::sync::Arc;
use flux_sdk::{FlowClient, dsl::*};
use serde_json::json;

# async fn ex(provider: Arc<dyn flux_provider::Provider>) -> flux_core::Result<()> {
let client = FlowClient::builder()
    .model("claude-sonnet-4-6")
    .auto_approve(true)
    .build(provider, ".")?;

// each $f in ["a.txt", "b.txt"] -> $contents: read $f ; return $contents
let flow = Flow::named("read_each")
    .body(|b| {
        b.each("f", lit(json!(["a.txt", "b.txt"])), |e| {
            e.collect("contents");
            e.body(|b| { b.call("read", [var("f")]); });
        });
        b.ret(var("contents"));
    })
    .build();

client.analyze(&flow).map_err(|d| flux_core::Error::Other(format!("{d:?}")))?;
let out = client.execute(&flow).await?;
println!("{}", out.result);
# Ok(()) }
```

The DSL is a **construction** convenience, not a type-checker: semantic validity (bounded loops,
top-level `await`, `match` subjects, op resolution) stays the analyzer's job — always `analyze` a built
flow before you `execute` it.

## Quick start — a conversational agent

```rust,ignore
use flux_sdk::Client;

# async fn ex(provider: Box<dyn flux_provider::Provider>) -> flux_core::Result<()> {
let client = Client::builder().auto_approve(true).build(provider, ".")?;
let out = client.run("Summarize the README").await?;
println!("{}", out.text);
# Ok(()) }
```

`Client` runs the same `FlowEngine` as the CLI. The model emits typed plans (or prose), and the
editable `agent-loop.flux` controls gather, execute, revise, and completion passes; there is no
separate provider-native tool loop.

## Direct-flow lifecycle and lower-level crates

`FlowClient` is the recommended AI-application API when the application owns a flow. It exposes
natural-language `compile`, deterministic `parse`/`parse_module`, `analyze`/`analyze_seeded`,
`optimize`, `execute`/`execute_with`, `execute_optimized`, and the `run`/`run_flow` convenience
pipelines. Its builder carries permission, approval, sandbox, and compiler-budget controls; the
client can register custom operations, packs, composite ops, artifact definitions, and sub-agents.

Use the lower-level published libraries only when you need to replace a host boundary:

- `codewandler-flux-lang` (`flux_lang`) owns the standalone AST, parser/formatter, analyzer,
  optimizer, schema, DSL, program declarations, and the reference interpreter over injected
  `OpHost`/`ValueStore`/`FlowSink` traits.
- `codewandler-flux-flow` (`flux_flow`) adapts the language onto providers, the guarded executor,
  event/value stores, replay, suspension, and voice. Its `FlowEngine` is the advanced embedding
  surface for cancellable or durable conversational turns and flow-driven sessions.

The [public SDK guide](https://codewandler.github.io/flux/docs/sdk/overview) maps these choices, and
the [FlowClient guide](https://codewandler.github.io/flux/docs/sdk/flow-client) documents the full
lifecycle, one-shot `await` boundary, result type, and voice split.

## Recipes

`flux_sdk::recipes` is a cookbook of reusable, parameterized flow builders on top of the DSL — hand any of
them to a `FlowClient` to `analyze` + `execute`:

| Module | Recipe(s) |
|---|---|
| `recipes::routing` | `route_intent` — classify once, then dispatch deterministically |
| `recipes::lookup` | `answer_with_fallback` — graceful degradation into a typed `Answer` |
| `recipes::batch` | `map_each`, `repeat_until`, `poll_for`, `race_first` — the loop family |
| `recipes::resilience` | `retry_with_backoff`, `with_timeout`, `with_budget`, `try_catch` |
| `recipes::fanout` | `parallel_all` — run ops concurrently |
| `recipes::dispatch` | `match_value` — dispatch on a computed value |
| `recipes::compose` | `resilient_call` — `retry { timeout { fallback {…} } }`, nested |

```rust,ignore
use flux_sdk::recipes::routing::route_intent;
use flux_sdk::dsl::lit;

let flow = route_intent(
    "intent.classify",
    lit("I'd like to book a flight"),
    &[("book", "booking.create")],
    "support.ticket",
);
// client.analyze(&flow)?; client.execute(&flow).await?;
```

## Providers

`flux-sdk` is provider-agnostic — pass any `flux_provider::Provider`. The concrete backends
live in `flux-providers` (modules `anthropic`/`openai`/`openrouter`/`ollama`) so the SDK stays light.

## License

MIT OR Apache-2.0.

[`Client`]: src/lib.rs
[`FlowClient`]: src/flow.rs
[`dsl`]: ../flux-lang/src/dsl.rs
