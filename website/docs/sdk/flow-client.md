---
title: FlowClient
---

# FlowClient

`FlowClient` is the SDK surface for the Flux-Lang lifecycle. It is the right entry point when you want
to parse or compile a flow, analyze it, optionally optimize it, and execute it through the flux runtime.
For the SDK as a whole — install, the other front doors, mock providers, recipes — start at the
[SDK overview](./overview.md).

Typical lifecycle:

```rust
let client = FlowClient::builder()
    .model("anthropic/opus")
    .auto_approve(true)
    .build(provider, ".")?; // provider: Arc<dyn flux_provider::Provider>

let ast = client.parse(source)?;            // deterministic text → AST (no model call);
                                            // client.compile(text, None).await? is the NL→AST partner
client.analyze(&ast)                        // Err carries Vec<Diagnostic>: unknown ops, unbound $vars
    .map_err(|d| flux_core::Error::Other(format!("{d:?}")))?;
let out = client.execute(&ast).await?;      // ExecutionResult: result, transcript, steps, tool_calls
```

`run(text)` is the one-call convenience pipeline (`compile → analyze → execute`); a failed analysis
aborts before any side effect.

Inputs can be seeded as flow variables for stored flows: `execute_with(&ast, inputs)` injects them
before the run, and `analyze_seeded(&ast, names)` analyzes the flow as it will actually run under that
seeding. Seeding data does not grant capabilities; operation dispatch still uses the same policy and
approval path.

Use the agent-facing `Client` when you want a complete conversational turn. Use `FlowClient` when you
already have a flow or want deterministic lifecycle control.
