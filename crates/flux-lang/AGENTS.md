# AGENTS.md — contributing to `flux-lang`

The contributor contract for the Flux-Lang **language core + reference interpreter**. This file refines
the workspace [`AGENTS.md`](../../AGENTS.md) for this crate; the workspace rules (commits, safety
envelope, layering lint) still apply.

## What this crate is

`flux-lang` is the **language**: the AST, its renderer/analyzer/schema, the generated skill, and a
**reference interpreter** that runs a flow against *injected* effect traits. It is an **L0 leaf** — it
depends only on other L0 contracts (`flux-core`, `flux-spec`, `flux-policy`, `flux-evidence`) plus
external crates (`serde`, `schemars`, `tokio`, …). It must **not** depend on `flux-runtime`,
`flux-agent`, `flux-session`, a provider, or any concrete tool. The `flux-flow` engine (L3) adapts its
safety envelope onto this crate's traits.

## The trait seam (don't break it)

The interpreter (`runtime.rs`) is generic over three L0 traits — never reach for a concrete engine type:

- `host::OpHost` — dispatch an op, look up the op catalog, request approval, trim output.
- `store::ValueStore` — store/resolve values and symbols, append the run-event trace, project the view.
- `sink::FlowSink` — stream observations (text/op-call/op-result/turn-end).

`store::MemStore` is an in-memory `ValueStore` so the interpreter runs standalone (CLI, tests). The
engine provides the real adapters (`ExecutorHost`, `SinkBridge`, `FlowStore: ValueStore`) in
`flux-flow`'s `runtime`/`state` modules, and re-exports `execute_flow`/`execute_call` with unchanged
signatures.

## Single source of truth — node kinds are generated

The `Node` enum's **doc-comments** in `src/ast.rs` are the canonical one-line node descriptions. Via
`schema::node_kind_catalog()` they generate: the planner-prompt grammar, the "Node kinds at a glance"
table in `docs/reference.md`, the `## Node kinds` table in `skill/SKILL.md`, and the same table in the
engine skill. **Never hand-edit a generated `<!-- BEGIN/END generated:node-kinds -->` block.** After
changing a `Node` variant or its doc-comment, regenerate:

```bash
UPDATE=1 cargo test -p flux-lang --test skill_in_sync          # language skill + docs/reference.md
UPDATE=1 cargo test -p flux-flow --test skill_docs_in_sync     # engine skill
```

Hand-written prose (the detailed per-node sections in `docs/reference.md`, the examples in `skill.rs`)
still needs manual updates in the same commit.

## Dev loop

```bash
cargo build -p flux-lang
cargo test  -p flux-lang                       # lib + interpreter + in-sync tests
cargo test  -p flux-lang --features cli        # also the fluxlang CLI tests
cargo clippy -p flux-lang --all-targets --features cli -- -D warnings
cargo test  -p flux-codegate                   # confirm flux-lang is still L0
cargo fmt --all
```

The `fluxlang` binary is gated behind the `cli` feature (so library consumers don't pull `clap`); build
or test it with `--features cli`.

## Roadmap

See [`docs/PRD.md`](docs/PRD.md). Near-term: the two writable display modes (a human-readable form and a
token-efficient form for future fine-tuning) and `fluxlang compile` (text → AST), which the renderer and
JSON wire form already anticipate.
