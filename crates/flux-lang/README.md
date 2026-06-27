# flux-lang

**Flux-Lang is a small programming language built for LLMs.** Instead of acting tool-by-tool, a model
expresses a whole task as a typed **execution graph** — an AST — and a deterministic runtime executes
it. The defining principle of the surrounding project holds here in miniature:

> **The LLM plans. The runtime runs.** The model is a compiler front-end that emits a readable plan;
> the runtime resolves *symbols* to stored *values* and runs registered *operations* under policy.

Designed-for-AI shows up in the language itself:

- **Simple, explicit control structures.** Loops (`repeat`/`each`/`loop`), branches (`when`/`unless`),
  error handling (`try`/`retry`), concurrency (`parallel`/`race`) — all are *nodes in the graph*, never
  hidden inside a shell string. A turn is a program you read like Go or Rust before it runs.
- **Token-efficient by construction.** Results are stored as **symbols** and referenced by name, so raw
  outputs aren't re-sent every step. The model sees a compact symbolic view, not a growing transcript.
- **Auditable & deterministic.** The plan is an artifact: it can be rendered, diffed, and re-run with
  the fewest possible model calls.
- **Operations are injected, not built in.** The language knows *node kinds*, not tools — a `call`
  targets whatever operations the host advertises. That keeps the language a pure, reusable core.

## What's in the crate

| Module | Role |
|---|---|
| `ast` | the typed AST (`Node`, `Value`, `TypeRef`, `FlowEffect`, …) — all derive `JsonSchema` |
| `render` | render an AST as a human-readable execution tree |
| `analyze` | validate a flow against an abstract op catalog (`OpCatalog`) |
| `schema` | the single source of truth: the AST's JSON Schema + the node-kind catalog |
| `skill` | the generated language skill an LLM reads to author Flux-Lang |
| `runtime` | the **reference interpreter** — runs a flow against injected `host`/`store`/`sink` traits |
| `host` / `store` / `sink` | the L0 trait seams the interpreter dispatches effects through |

It is an **L0 leaf**: it depends only on other pure contracts (`flux-core`, `flux-spec`, `flux-policy`,
`flux-evidence`) — no provider, no concrete runtime, no tools. All effects are injected via traits, so
an embedder (the `flux-flow` engine) adapts its safety envelope onto `OpHost`/`ValueStore`/`FlowSink`.

## The `fluxlang` CLI

Inspect the language without the engine (built with `--features cli`):

```bash
cargo run -p flux-lang --features cli --bin fluxlang -- skill    # the language skill (markdown)
cargo run -p flux-lang --features cli --bin fluxlang -- schema   # the AST JSON Schema
echo '<json-ast>' | cargo run -p flux-lang --features cli --bin fluxlang -- render   # AST → tree
```

## Docs

- [`docs/reference.md`](docs/reference.md) — every node kind, fields, semantics (node table generated).
- [`docs/syntax.md`](docs/syntax.md) — the writable text-syntax spec.
- [`docs/PRD.md`](docs/PRD.md) — design rationale, scope, and roadmap (the two display modes + parser).
- [`examples/call-routing.flux`](examples/call-routing.flux) — a worked text-syntax example (a model-backed intent step plus deterministic routing).
- [`AGENTS.md`](AGENTS.md) — contributor contract for this crate.
