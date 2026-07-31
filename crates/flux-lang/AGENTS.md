# AGENTS.md — contributing to `flux-lang`

The contributor contract for the Flux-Lang **language core + reference interpreter**. This file refines
the workspace [`AGENTS.md`](../../AGENTS.md) for this crate; the workspace rules (commits, safety
envelope, layering lint) still apply.

## What this crate is

`flux-lang` is the **language**: the AST, its renderer, the round-trippable **text syntax**
(`parse`/`format`), the analyzer (`lower` → typed HIR with arg type-checking), the **optimizer**
(`optimize` → `PhysicalPlan`), the artifact **prelude**, the multi-agent **`program`** layer, the
schema/skill SSOT, and a **reference interpreter** that runs a flow against *injected* effect traits. It is an **L0 leaf** — it
depends only on other L0 contracts (`flux-core`, `flux-spec`, `flux-policy`, `flux-evidence`) plus
external crates (`serde`, `schemars`, `tokio`, …). It must **not** depend on `flux-runtime`,
`flux-agent`, `flux-events`, a provider, or any concrete tool. The `flux-flow` engine (L3) adapts its
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
`schema::node_kind_catalog()` they generate the "Node kinds at a glance" table in
`docs/reference.md`, and the `## Node kinds` table in the explicitly installable
`skill/SKILL.md`. **Never hand-edit a generated `<!-- BEGIN/END generated:node-kinds -->` block.** After
changing a `Node` variant or its doc-comment, regenerate:

```bash
UPDATE=1 cargo test -p codewandler-flux-lang --test skill_in_sync          # language skill + docs/reference.md
UPDATE=1 cargo test -p codewandler-flux-lang --test website_in_sync        # public website tables
```

Hand-written prose (the detailed per-node sections in `docs/reference.md`, the examples in `skill.rs`)
still needs manual updates in the same commit.

## Dev loop

```bash
cargo build -p codewandler-flux-lang
cargo test  -p flux-lang                       # lib + interpreter + in-sync tests
cargo test  -p flux-lang --features cli        # also the fluxlang CLI tests
cargo clippy -p codewandler-flux-lang --all-targets --features cli -- -D warnings
cargo test  -p flux-codegate                   # confirm flux-lang is still L0
cargo fmt --all
```

The `fluxlang` binary is gated behind the `cli` feature (so library consumers don't pull `clap`); build
or test it with `--features cli`.

⚠ **`cargo test --workspace` does not reach the `--features cli` line above** — no workspace member
enables the feature, so the whole `fluxlang` test target is invisible to the default gate. That is
how C-308 happened: L-96 made `confirm "y", risk: high` canonical, a `fluxlang` test using it as
*malformed* input started panicking, and every gate stayed green for days. Since C-308 the CI `check`
job runs `scripts/check-feature-gated-tests.sh`, which owns this leg (and the other feature-gated
targets in both workspaces) — so a break here reds CI now. Keep the `--features cli` line in your
local loop anyway: it is the fast feedback, CI is the backstop.

**Fixtures that must stay malformed must be malformed *lexically*.** The vocabulary of valid
statements grows; an unterminated string literal never becomes valid. A fixture that is only "a
spelling the parser happens to reject today" is a fixture the next syntax story silently invalidates.

## Design & planning docs

The full map of flux-lang design / spec / plan docs — read the relevant one before changing behaviour,
and keep it in sync with your change (design + status + plan move together):

**Language spec & reference**
- [`docs/PRD.md`](docs/PRD.md) — the immutable, source-of-record PRD (**don't edit**; track progress in `STATUS.md`).
- [`docs/reference.md`](docs/reference.md) — every node kind, fields, semantics (node table generated).
- [`docs/syntax.md`](docs/syntax.md) — the writable text-syntax spec.
- [`docs/railflux.md`](docs/railflux.md) — the Railflux ASCII dataflow projection (`src/rail.rs`, `fluxlang rail`). **Output spec only** — the reader is deferred (L-100), so treat this file as the contract that shape must hold to.
- [`README.md`](README.md) — what the execution layer *is* (and isn't) + the crate overview.

**Forward design — the evolution**
- [`../../docs/designs/flux-lang-evolution.md`](../../docs/designs/flux-lang-evolution.md) — the agent-cognition AST (`ctx`/`need` + artifact ontology), language/syntax, candidate control-flow primitives, the multi-agent `Program` layer, and the SDK.
- [`docs/design-review.md`](docs/design-review.md) — the design review of that doc.
- [`docs/STATUS.md`](docs/STATUS.md) — PRD conformance matrix (built vs. planned, with evidence).
- [`docs/evolution-impl-plan.md`](docs/evolution-impl-plan.md) — the phased build plan for the evolution.

**Engine (L3, builds on this crate)**
- [`../../docs/designs/flux-flow.md`](../../docs/designs/flux-flow.md) — the flux-flow engine design.

**Local WIP plans** — under `.flux/plans/` (gitignored, author's machine only): `ast-node-expansion.md`
(superseded), `flow-new-primitives.md`, `flux-flow-implementation.md`.

The text↔AST layer (`parse`/`format`) is built and round-trips, the compact display is available for
workbench experiments, and `fluxlang compile` maps authored source to JSON AST. The default agent no
longer consumes either schema as a model-emission contract.
