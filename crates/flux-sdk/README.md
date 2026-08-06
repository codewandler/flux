# flux-sdk

The high-level library API for [flux](https://github.com/codewandler/flux) — embed a tool-enabled,
policy-gated agent in your own Rust program. You supply a `Provider` (a model backend) and a workspace
root; the SDK wires the agent loop, the built-in tools, the safety envelope, and a session.

The guiding idea is **"the LLM is not the runtime"**: typed model stages may interpret intent and
propose native operation calls, but an authored Flux-Lang loop and deterministic engine own control
flow, approval, and execution through a non-bypassable safety envelope. The model never emits Flux.
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
| [`FlowClient`] | The direct authored Flux-Lang lifecycle: parse, analyze, optimize, seed, and execute. | `examples/parameterized_flow.rs` |
| [`dsl`] | Author the AST **in Rust** — builder primitives (loops + control-flow) that compile to the Flux-Lang AST, then run via `FlowClient`. | `examples/dsl_loops.rs` |

All three examples are hermetic (a mock provider) and run with no API key:

```sh
cargo run -p codewandler-flux-sdk --example dsl_loops      # build loops/control-flow with the DSL, execute them
cargo run -p codewandler-flux-sdk --example client_basic   # the self-hosted Flux-Lang agent loop
cargo run -p codewandler-flux-sdk --example parameterized_flow # parse authored Flux → execute
```

A fourth SDK product area builds on the same engine instead of introducing a parallel test harness:
**Agent Lab** records real agent runs as deterministic fixtures, replays them offline in tests, runs
world-pinned what-if experiments, and resurrects interrupted durable turns. See
[`examples/agent_lab.rs`](examples/agent_lab.rs) and the public
[Agent Lab guide](https://codewandler.github.io/flux/docs/sdk/agent-lab).

Additional hermetic examples cover domain flows and both datasource shapes, with no API key:

```sh
cargo run -p codewandler-flux-sdk --example intent_routing # classify an utterance, then `route` to a handler
cargo run -p codewandler-flux-sdk --example faq_lookup     # KB lookup + `fallback` escalation → a typed `Answer`
cargo run -p codewandler-flux-sdk --example datasource_recipe # ingest and query an indexed knowledge backend
cargo run -p codewandler-flux-sdk --example live_datasource # attach an async live system of record
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

`Client` runs the same `FlowEngine` as the CLI. The default authored loop performs typed intent
routing, native-schema exploration, batch approval/execution, local repair, and presentation. Select
another loop explicitly with `ClientBuilder::agent_loop`; an ejected `agent-loop.flux` is not loaded
merely because the file exists.

Every client resolves that selection to an `AgentLoopBinding` before its first provider call. An
omitted selection becomes the explicit `adaptive@1` binding; embedders that already admitted an
exact authored loop can pass `ClientBuilder::agent_loop_binding`. The binding includes bounded
profile, revision, source digest, runner, entry point and runtime requirements, while retaining the
validated source in-process. Reusing a durable session with a different binding is refused rather
than silently changing its behavior.

Custom typed stages are ordinary guarded operations. `stage_fn::<I, O, _, _, _>` derives unrelated
input and output schemas, so the Flux analyzer sees the real `O` type at call sites:

```rust,ignore
let client = Client::builder()
    .register_op(flux_sdk::stage_fn(
        "classify_ticket",
        "Classify one support ticket",
        |input: Ticket| async move { Ok::<Classified, String>(classify(input)) },
    ))
    .build(provider, ".")?;
```

Because a flux run is durable plan source plus redacted cassette cells, the SDK also gives embedders
an Agent Lab instead of a mock-only test story: record a live turn once, replay it offline in
`cargo test`, run controlled what-if changes against the frozen world, and finish an interrupted
turn without re-calling the model or re-firing recorded side effects. The Lab is documented in the
[Agent Lab guide](https://codewandler.github.io/flux/docs/sdk/agent-lab) and exercised by
`examples/agent_lab.rs`.

## Datasources: indexed knowledge and live systems

Flux keeps two datasource shapes separate because they own different data and paging semantics:

- **Indexed knowledge** implements `flux_capabilities::DatasourceBackend`. Records are ingested into
  a local memory, SQLite, Postgres, or semantic index, then the generic
  `search`/`get`/`list`/`relation`/`batch_get`/`sources` pack is attached with
  `ClientBuilder::try_register_pack` or `FlowClient::try_register_pack`. See
  `examples/datasource_recipe.rs`.
- **Live systems of record** implement the SDK-re-exported `flux_sdk::datasource::LiveDatasource`.
  The conversational builder's fallible `try_with_live_datasource(domain, backend)` installs the
  generated `<domain>.list` and `<domain>.get` operations together with their evidence group and
  configured-domain signal. See `examples/live_datasource.rs`.

A live backend declares its entity/filter/page schema and any `LiveAccess::Network` or
`LiveAccess::Connection` resources. Registration snapshots that contract. Plan preview and dispatch
then derive the same exact requirements: `datasource.read` on `<domain>/<entity>` plus each declared
backend resource. Filters, cursors, row IDs, and weak references never become authority grants, and
real backend IO still uses the guarded host surfaces supplied through `ToolContext`.

## Typed user interaction

Conversational clients can install an `Arc<dyn flux_sdk::interaction::UserInteraction>` with
`ClientBuilder::with_user_interaction`. This conditionally registers and pre-allows `user.ask`; an
explicit deny remains authoritative. The handler receives redacted prompt text plus a bounded JSON
Schema and returns either a reviewed JSON value or `Cancelled`. Flux validates the schema before the
handler runs and validates the response again afterward.

The interaction contract is not an approval API: no answer can authorize an effect. Audio-capable
SDK hosts may advertise `InteractionCapabilities::with_audio()` and resolve opaque
`PromptAudioRef` asset ids internally. Raw audio cannot appear in `InteractionResponse`. The stock
CLI and TUI advertise text controls only.

## Authorization and approval

Every `Client` and `FlowClient` has a mandatory authorization profile. The default is the documented
local single-user profile (`ExecutionAuthorization::local()`), which carries both the local grants
and a resolved local identity. A service that authenticates multiple callers should install its
resolved policy, caller, and trust explicitly:

```rust,ignore
use flux_sdk::{authorization::{AuthorizationPolicy, Caller, Trust}, Client};

# fn ex(
#     provider: Box<dyn flux_provider::Provider>,
#     policy: AuthorizationPolicy,
#     caller: Caller,
#     trust: Trust,
# ) -> flux_core::Result<()> {
let client = Client::builder()
    .with_authorization(policy, caller, trust)
    .auto_approve(true)
    .build(provider, ".")?;
# Ok(()) }
```

Authorization is evaluated before permission rules and approval. Consequently, `auto_approve(true)`
can skip an approval prompt but cannot grant an action denied by the policy. The same profile is
carried into direct-flow, streamed, voice, and sub-agent execution paths.

Long-lived services that reuse one engine across authenticated principals must not retarget its
executor. Freeze the request identity and pass it with the turn instead:

```rust,ignore
use flux_sdk::authorization::TurnIdentity;

let identity = TurnIdentity::new(caller, trust);
client
    .engine()
    .run_turn_as(session_id, input, sink, identity)
    .await?;
```

`run_turn_as` (and its cancellable/authored-flow counterparts) installs the identity only after the
engine acquires its single-active-turn gate. Policy checks, approval receipts, audit rows, and
spawned specialists therefore share one immutable request identity.

## Operation registration collisions

Operation names are unique identities. Client construction returns an error when a custom operation
or plugin collides with a built-in, another plugin, or an earlier custom operation; the diagnostic
names both registration sources. `FlowClient::try_register_op` is the fallible direct-registration
API for integrations. The older `FlowClient::register_op` convenience method remains source
compatible but panics on an invalid or duplicate declaration, so new production code should use the
fallible form. Pack installers should likewise expose a fallible function and use
`ClientBuilder::try_register_pack` or `FlowClient::try_register_pack`; the legacy `register_pack`
wrappers remain source-compatible but cannot propagate a collision. Runtime owners that
intentionally replace a control-plane operation must use the separately named
`ToolRegistry::replace_from`; ordinary registration never overwrites implicitly.

## Direct-flow lifecycle and lower-level crates

`FlowClient` is the recommended AI-application API when the application owns a flow. It exposes
deterministic `parse`/`parse_module`, `analyze`/`analyze_seeded`,
`optimize`, `execute`/`execute_with`, `execute_optimized`, and the `run_flow` convenience pipeline.
Its builder carries permission, approval, and sandbox controls; the
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
