# flux-sdk

The high-level library API for [flux](https://github.com/codewandler/flux) — embed a tool-enabled,
policy-gated agent in your own Rust program. You supply a `Provider` (a model backend) and a workspace
root; the SDK wires the agent loop, the built-in tools, the safety envelope, and a session.

The guiding idea is **"the LLM is not the runtime"**: the model emits a Flux-Lang plan (an execution
graph), and a deterministic engine runs it through a non-bypassable safety envelope.

## Three front doors

| Surface | What it is | Example |
|---|---|---|
| [`Client`] | The classic agent loop: stream a turn, let the model call tools under the envelope. | `examples/client_basic.rs` |
| [`FlowClient`] | The Flux-Lang lifecycle: `compile` an instruction into a typed AST, `analyze` it, `execute` it. | `examples/flow_compile.rs` |
| [`dsl`] | Author the AST **in Rust** — builder primitives (loops + control-flow) that compile to the Flux-Lang AST, then run via `FlowClient`. | `examples/dsl_loops.rs` |

All three examples are hermetic (a mock provider) and run with no API key:

```sh
cargo run -p flux-sdk --example dsl_loops      # build loops/control-flow with the DSL, execute them
cargo run -p flux-sdk --example client_basic   # the classic agent loop
cargo run -p flux-sdk --example flow_compile   # NL → AST → execute
```

Two **domain** examples show the DSL on real tasks, with the model/datasource adapters mocked (registered
stub ops) so they run with no API key:

```sh
cargo run -p flux-sdk --example intent_routing # classify an utterance, then `route` to a handler
cargo run -p flux-sdk --example faq_lookup     # KB lookup + `fallback` escalation → a typed `Answer`
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

## Quick start — the classic agent

```rust,ignore
use flux_sdk::Client;

# async fn ex(provider: Box<dyn flux_provider::Provider>) -> flux_core::Result<()> {
let client = Client::builder().auto_approve(true).build(provider, ".")?;
let out = client.run("Summarize the README").await?;
println!("{}", out.text);
# Ok(()) }
```

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

[`Client`]: https://docs.rs/flux-sdk/latest/flux_sdk/struct.Client.html
[`FlowClient`]: https://docs.rs/flux-sdk/latest/flux_sdk/struct.FlowClient.html
[`dsl`]: https://docs.rs/flux-lang/latest/flux_lang/dsl/index.html
