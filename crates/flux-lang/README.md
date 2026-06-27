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

## What the execution layer is — and isn't

This is about the **execution layer** — how the runtime *runs* a flow — not the agent/app spec layer
(agents, channels, journeys; see the [evolution design](../../docs/designs/flux-lang-evolution.md)).

A flow is a **typed AST of structured control flow**, and the reference interpreter (`runtime.rs`) is a
**tree-walking interpreter**: it walks a flow's `body` top to bottom, descending into nested branch/loop
bodies, threading **immutable, single-assignment** values bound to symbols, with every op traversing one
policy/approval envelope and every step logged. Precisely:

- **Structured, not free-form.** Sequence + selection (`when`/`unless`) + **bounded** iteration
  (`repeat`/`each`/`loop`) + explicit concurrency (`parallel`/`race`). **No `goto`, no recursion, no
  unbounded loop, no mutable variable** — Dijkstra-style structured programming.
- **Single-assignment (SSA-like).** A symbol names one immutable value; a revision is a new value id
  (`$draft@1 → @2`). Because values are write-once, the **data dependencies between steps form a DAG**.
- **Deterministic by default.** Which step runs, in what order, under which guards, is fixed and
  analyzer-validated. Only steps marked **`!model`** are non-deterministic — and even those are bound to a
  declared output schema and **cannot change control flow**.

So — a behaviour tree? a DAG? Neither, exactly:

| It is **not**… | …because |
|---|---|
| a **behaviour tree** | no tick loop, no Success/Failure/Running status, no reactive re-evaluation from a root — a flow runs once, it isn't re-ticked every frame |
| a **hand-wired DAG / dataflow graph** (Airflow-style) | you write structured *code*; the dependency **DAG is *derived*** from single-assignment data, not drawn by hand |
| a **general-purpose language** | no recursion, unbounded loops, mutable state, or goto (deliberate — PRD §4) |
| a **state machine / actor** | no explicit states/transitions; `await` is suspend/resume, not an FSM |
| an **LLM agent loop** (ReAct) | the model **compiles** the plan once; the runtime schedules it — the model is not the scheduler |

In one line:

> A **deterministic workflow engine** whose programs are typed ASTs of structured control flow, run by a
> tree-walking interpreter over single-assignment values (whose dependencies form a DAG that licenses
> parallel/cache optimization), where only explicitly-marked, schema-constrained `!model` leaves are
> non-deterministic.

Closest familiar relatives: a **Temporal / Step-Functions**-style deterministic orchestration engine
(deterministic workflow + non-deterministic "activities"), but in-process and authored as a typed AST
rather than a wired graph; an **SSA IR** for the value model; a **build system** for the content-hash
caching / dead-step elimination the optimizer targets.

> *Today* the interpreter runs the AST **sequentially** (`parallel`/`race` are the explicit concurrency
> escape hatches); the data-dependency DAG is the basis for the **planned** optimizer (parallelize
> independent ops, cache by input hash, drop dead steps — PRD §15), not a claim that execution is already
> graph-scheduled.

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
- [`docs/STATUS.md`](docs/STATUS.md) — PRD conformance matrix (what's built vs. planned, with evidence).
- [`../../docs/designs/flux-lang-evolution.md`](../../docs/designs/flux-lang-evolution.md) — forward design: the agent-cognition AST (`ctx`/`need` + artifact ontology), language/syntax, **candidate control-flow primitives**, and SDK.
- [`docs/evolution-impl-plan.md`](docs/evolution-impl-plan.md) — phased implementation plan for the above.
- [`examples/call-routing.flux`](examples/call-routing.flux) — a worked text-syntax example (a model-backed intent step plus deterministic routing).
- [`AGENTS.md`](AGENTS.md) — contributor contract for this crate.
