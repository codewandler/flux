# Agent-owned journeys and app capability ceilings

## Problem

An app can currently write `journey answer / agent guide`, but the runtime discards that ownership
and executes the body with the app's default model, unscoped datasource tools, and a hard-coded safe
allowlist. Conversely, an agent-bound trigger inherits its declaration but leaves retrieval to model
choice. This makes the beginner docs assistant a useful demonstration, but not a reliable grounded
application.

## Design

Programs gain an optional top-level capability declaration and agents gain an optional narrowing:

```flux
permissions
  allow [search, "ai.reason", send]
  deny [write, edit, bash]

agent guide
  tools [search]
  datasources [handbook]
  allow [search, "ai.reason", send]
  deny [write, edit, bash]
```

Entries are exact registered operation names. The top-level allow-list is a registry ceiling; an
agent allow-list intersects it, while denies union and always win. A missing allow inherits its
parent; `allow []` is explicitly empty. Local subject-scoped permission rules remain a separate
approval layer inside the source ceiling: local deny wins, local allow may approve a remaining
subject, and auto-approval may approve only calls still present in the narrowed registry.

`tools` stays independent: it controls what an open-ended agent sees. Authored journey calls use the
effective capability set even when an operation is intentionally absent from the agent's model-facing
catalog.

An owned journey remains a fresh deterministic flow for every event. The host resolves its owner
before execution and reuses the agent mapping for model, persona, and datasource wrappers. Cognition
operations are rebound to that model and receive the persona before their operation-specific contract.
The journey does not become a conversational agent loop.

## Validation and compatibility

The CLI uses a fallible app constructor which validates owners, trigger targets, datasource names,
tool/capability names, and every recursively nested call before starting channels. Composite bodies
are checked transitively. Diagnostics name the declaration and rejecting capability layer.

Existing infallible constructors remain available. Programs without capability declarations keep
the legacy safe journey allowlist and agent `tools`-as-grants behavior. The added public `Program` and
`AgentDecl` fields are nevertheless a pre-1.0 breaking Rust API change and require the next release to
be a minor bump.

## Tutorial

The app capstone first runs the agent-bound trigger and calls out that a prompt can request retrieval
but cannot make it structural. The same event is then routed to an owned journey with an explicit
`search -> ai.reason -> send` graph and declared capabilities. Success means every completed answer
has traversed scoped retrieval; only wording and interpretation remain model-controlled.
