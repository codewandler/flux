---
title: FlowClient
description: "Lifecycle details for deterministic flow execution: parse/compile/analyze/execute with optional seeded inputs."
---

# FlowClient

`FlowClient` is the SDK surface for the Flux-Lang lifecycle. Use it when you already have a flow, or
when your application wants explicit control over compile, parse, analyze, and execute steps.

For the SDK as a whole — install, `Client`, mock providers, recipes, and the Rust DSL — start at the
[SDK overview](./overview.md).

## Typical lifecycle

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

## Related docs

- [SDK overview](./overview.md) — provider setup and the higher-level `Client`.
- [Tooling](../language/tooling.md) — CLI equivalents for flow execution.
- [Safety and approvals](../agent/safety.md) — policy and approval behavior during SDK dispatch.
