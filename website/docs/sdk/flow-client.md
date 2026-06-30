---
title: FlowClient
---

# FlowClient

`FlowClient` is the SDK surface for the Flux-Lang lifecycle. It is the right entry point when you want
to parse or compile a flow, analyze it, optionally optimize it, and execute it through the flux runtime.

Typical lifecycle:

```rust
let ast = client.parse(source)?;
client.analyze(&ast)?;
let out = client.execute(&ast).await?;
```

Inputs can be seeded as values for stored flows. Seeding data does not grant capabilities; operation
dispatch still uses the same policy and approval path.

Use the agent-facing `Client` when you want a complete conversational turn. Use `FlowClient` when you
already have a flow or want deterministic lifecycle control.
