---
title: SDK overview
description: "Entry point for embedding flux in Rust, including loops, providers, tooling, and safety expectations."
---

# Embedding flux: the Rust SDK

`flux-sdk` embeds the same policy-gated agent engine used by the CLI. You provide a model provider
and a workspace root; the SDK wires the agent loop, built-in tools, safety envelope, and session
storage.

Use the SDK when you want flux behavior inside your own Rust application instead of shelling out to
the `flux` binary.

## Install

The SDK and its dependency closure are published on crates.io under the `codewandler-` prefix (the
short `flux-*` package names are owned by unrelated projects):

```bash
cargo add codewandler-flux-sdk codewandler-flux-providers
```

Package names are prefixed, but Rust import paths stay short: `use flux_sdk::…` and
`use flux_providers::…`. `flux-sdk` is provider-agnostic and has no cargo features of its own. The
concrete backends live in `flux-providers` (Anthropic, OpenAI, OpenRouter, Ollama, AWS Bedrock, and
the claude/codex subscription providers; the full-duplex voice provider sits behind its `realtime`
feature).

## The three front doors

| Surface | What it is |
|---|---|
| `Client` | The classic agent loop: run a turn, let the model plan and call tools under the envelope. |
| `FlowClient` | The Flux-Lang lifecycle: `compile` an instruction into a typed AST, `analyze` it, `execute` it. |
| `dsl` | Author the AST **in Rust** — builder primitives that compile to the Flux-Lang AST, then run via `FlowClient`. |

### `Client` — a conversational turn

```rust
let provider = Box::new(flux_providers::anthropic::anthropic_from_env()?);
let client = flux_sdk::Client::builder()
    .model("anthropic/opus")
    .build(provider, ".")?;
let out = client.run("Summarize the README").await?;
println!("{}", out.text); // TurnOutput: text, tool_calls, usage
```

The builder carries the policy: `allow`/`deny` permission rules (reads are pre-allowed,
everything else denies by default — there is no approval UI in a library), `auto_approve(true)`
for headless use, plus `system_prompt`, `max_tokens`, `max_iterations`, and `add_context` for
inline knowledge blocks.

### `FlowClient` — the Flux-Lang lifecycle

```rust
let client = flux_sdk::FlowClient::builder()
    .model("anthropic/opus")
    .auto_approve(true)
    .build(provider, ".")?; // provider: Arc<dyn flux_provider::Provider>

let ast = client.compile("read the doc and show it", None).await?; // NL → typed AST
client.analyze(&ast).map_err(|d| flux_core::Error::Other(format!("{d:?}")))?;
let out = client.execute(&ast).await?; // ExecutionResult: result, transcript, steps, tool_calls
```

`parse` is the deterministic (no model round-trip) partner of `compile` for stored flow text,
and `run` is the one-call `compile → analyze → execute` pipeline. See
[FlowClient](./flow-client.md) for the full lifecycle, including seeded inputs.

### `dsl` — build flows in Rust

Loops (`each`/`repeat`/`loop_for`/`race`) and the control-flow guards
(`match`/`route`/`fallback`/`timeout`/`budget`) are first-class builder primitives:

```rust
use flux_sdk::{FlowClient, dsl::*};
use serde_json::json;

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
```

The DSL is a *construction* convenience, not a type-checker — always `analyze` a built flow
before you `execute` it.

## Zero-key testing with a mock provider

`Provider` is a small trait (a name and a `stream` method), so tests and examples can run the
real engine and envelope hermetically — no API key, no network:

```rust
struct OneShotMock { chunks: Mutex<Option<Vec<Chunk>>> }

#[async_trait]
impl Provider for OneShotMock {
    fn name(&self) -> &str { "mock" }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        let chunks = self.chunks.lock().unwrap().take().unwrap_or_default();
        Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
    }
}
```

Every SDK example ships this way and runs with no API key:

```sh
cargo run -p codewandler-flux-sdk --example client_basic   # the classic agent loop
cargo run -p codewandler-flux-sdk --example flow_compile   # NL → AST → execute
cargo run -p codewandler-flux-sdk --example dsl_loops      # loops/control-flow via the DSL
```

`crates/flux-sdk/examples/client_basic.rs` is the full mock-provider turn;
`crates/flux-sdk/examples/dsl_loops.rs` executes a real `each` loop through the envelope against
a temp workspace.

## Recipes — a cookbook of prebuilt flows

`flux_sdk::recipes` is a cookbook of reusable, parameterized flow builders on top of the DSL —
hand any of them to a `FlowClient` to `analyze` + `execute`:

| Module | Recipe(s) |
|---|---|
| `recipes::routing` | `route_intent` — classify once, then dispatch deterministically |
| `recipes::lookup` | `answer_with_fallback` — graceful degradation into a typed `Answer` |
| `recipes::batch` | `map_each`, `repeat_until`, `poll_for`, `race_first` — the loop family |
| `recipes::resilience` | `retry_with_backoff`, `with_timeout`, `with_budget`, `try_catch` |
| `recipes::fanout` | `parallel_all` — run ops concurrently |
| `recipes::dispatch` | `match_value` — dispatch on a computed value |
| `recipes::compose` | `resilient_call` — `retry { timeout { fallback {…} } }`, nested |

```rust
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

The same cookbook is exposed on the CLI as `flux preset` — `flux preset list` shows it,
`flux preset help retry_with_backoff` describes one, and `--run` executes a filled preset.

## Where to go next

- The crate README in-repo: [`crates/flux-sdk/README.md`](https://github.com/codewandler/flux/blob/main/crates/flux-sdk/README.md)
- Runnable examples: [`crates/flux-sdk/examples/`](https://github.com/codewandler/flux/tree/main/crates/flux-sdk/examples)
- [FlowClient](./flow-client.md) — the `compile → analyze → execute` lifecycle in detail
- [Safety & approvals](../agent/safety.md) — the envelope applies identically when embedded
- [Flux-Lang overview](../language/overview.md) — the plan language your flows compile to

## Related docs

- [FlowClient](./flow-client.md) — deterministic Flux-Lang lifecycle control.
- [CLI](../agent/cli.md) — the binary surface backed by the same engine.
- [Safety & approvals](../agent/safety.md) — the runtime envelope embedded applications inherit.
