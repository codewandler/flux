# flux-recipes

A cookbook of reusable **Flux-Lang flow recipes**, authored with the Rust DSL. It is the first real
in-repo consumer of [`flux-sdk`](../flux-sdk) — every recipe is a small, parameterized function that
builds a `DraftAst` out of the DSL primitives, ready to `analyze` + `execute` through a `FlowClient`.

## Recipes, not the engine

Two crates, two roles — don't confuse them:

| Crate | Role |
|---|---|
| [`flux-flow`](../flux-flow) | the **engine** — the `compile → analyze → execute` lifecycle that *runs* a flow |
| **`flux-recipes`** (this crate) | a **cookbook** — pre-built flows you *run on* that engine |

`flux-flow` runs flows; `flux-recipes` is content for it.

## The recipes

| Module | Primitive | Recipe(s) |
|---|---|---|
| `routing` | `route` | `route_intent` — classify once, then dispatch deterministically to a handler |
| `lookup` | `fallback` + `Answer` | `answer_with_fallback` — graceful degradation into a typed answer |
| `batch` | `each` / `repeat` / `loop` / `race` | `map_each`, `repeat_until`, `poll_for`, `race_first` |

```rust,ignore
use std::sync::Arc;
use flux_recipes::dsl::*;
use flux_recipes::routing::route_intent;
use flux_sdk::FlowClient;

# async fn ex(provider: Arc<dyn flux_provider::Provider>) -> flux_core::Result<()> {
let client = FlowClient::builder()
    .model("claude-sonnet-4-6")
    .auto_approve(true)
    .build(provider, ".")?;

// route( intent.classify(utterance) ) { case "book" -> booking.create ; default -> support.ticket }
let flow = route_intent(
    "intent.classify",
    lit("I'd like to book a flight"),
    &[("book", "booking.create")],
    "support.ticket",
);

client.analyze(&flow).map_err(|d| flux_core::Error::Other(format!("{d:?}")))?;
let out = client.execute(&flow).await?;
println!("{}", out.result);
# Ok(()) }
```

Recipes are a **construction** convenience, not a type-checker: semantic validity (op resolution, bounded
loops, `route` labels) stays the analyzer's job — always `analyze` a built flow before you `execute` it.

## Tests

`tests/flows.rs` builds, analyzes, and executes every recipe against mocked adapter ops (registered stub
`Tool`s) and a never-called provider — hermetic, no API key:

```sh
cargo test -p flux-recipes
```

## License

MIT OR Apache-2.0.
