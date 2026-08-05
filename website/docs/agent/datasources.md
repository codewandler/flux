---
title: Datasources
description: "The agent's governed data layer: indexed knowledge and async live systems of record."
---

# Datasources

A **datasource** is a named, declared, **read-only** record surface the agent reaches through
[operations](../language/ops.md). Operations *do*; datasources *know* — anything that mutates is
not a datasource. Every datasource has one declared access mode:

| | Indexed knowledge | Live system of record |
|---|---|---|
| Data lives | In a flux-owned index of records | In an external API, database, or in-process backend |
| Best for | Searchable docs and contributed knowledge | Current tickets, customers, inventory, and similar domain data |
| Read shape | Search, address lookup, relations, offset paging | Typed entity filters, cursor paging, stable-id lookup |
| Writes | No | No |
| Operations | `sources`, `search`, `get`, `list`, `relation`, `batch_get` | `<domain>.list`, `<domain>.get` |

The split is intentional. A stable local snapshot benefits from indexing and ranked search; a
changing system of record needs async calls and backend-owned continuation cursors. The two access
modes stay two contracts — what they share is identity and the read-only definition. Work that is
claimed and moved therefore is not a datasource at all but a separate [work-board](#work-boards)
contract with an enforced state machine. None is a side channel: datasource reads and board
operations enter the ordinary operation catalog and cross authorization → approval → guarded IO.

## Datasources vs. operations

- An **operation** is the universal callable unit—the verbs of the system. Every tool, plugin
  operation, toolchain command, cognition op, and datasource read uses the same catalog and safety
  envelope.
- A **datasource** defines the data and access contract, and it is read-only by definition. An
  indexed datasource owns records; a live datasource owns an entity/filter/page schema and a
  host-side backend.
- The agent reaches either form **through operations**. Indexed retrieval uses the common operations
  listed below;
  registering a live domain named `support` generates `support.list` and `support.get`.

Plugins can participate on both sides: they may project callable operations and contribute records
to an index. A host can separately implement a `LiveDatasource` for on-demand reads from a system
of record.

:::note Datasources are not endpoints
An [endpoint](./endpoints.md) is a weak description of a service connection consumed by an
operation such as `sql.query`. A live datasource is a typed domain projection over a host-owned
backend. That backend may declare an exact network or connection target, but registering an endpoint
alone does not create indexed records or a `<domain>.list`/`<domain>.get` surface.
:::

## Indexed knowledge

Indexed datasources hold records that flux can rank, search, and traverse without repeatedly
reading the original files or putting an entire corpus into the prompt.

### Records

Each record is addressed by `(source, entity, id)`:

- **`source`**—where it came from: `local` for workspace docs, a declared datasource name, or an
  integration such as `gitlab`.
- **`entity`**—the record type, for example `file.document`, `openapi.operation`, or
  `gitlab.merge_request`.
- **`id`**—stable within its `(source, entity)`.

A record carries a short `title`, indexed `body`, free-form `meta` (URL, path, `updated_at`, …), and
typed `links` to other records. Retrieval can therefore follow relations—such as merge requests
linked from an issue—in addition to matching keywords.

### How knowledge gets in

Three routes feed the index:

1. **Workspace auto-index.** The CLI agent walks the workspace at startup and indexes documentation
   files (`.md`, `.txt`, `.rst`, `.adoc`, `.mdx`; capped in count and size) under the `local` source
   as `file.document` records.
2. **Program declarations.** A [multi-agent program](./programs.md) declares knowledge explicitly:

   ```flux
   datasource docs
     # Use "markdown" for a directory of docs, or "openapi" for an API spec file.
     kind "markdown"
     path "./docs"
   ```

   A relative path resolves against the program file's own directory, not the directory from which
   `flux app run` was launched. An absolute path is used as-is.

   Only knowledge kinds are ingested here. A kind that names neither a knowledge ingester nor a
   [work board](#work-boards) is a startup error naming the kinds that exist—a misspelled kind never
   falls back to a default, because a datasource silently bound to the wrong port is worse than one
   that refuses to start.
3. **Plugin records.** A [plugin](../plugins/authoring.md) declares datasources in its manifest and
   emits records through the gated `datasource.*` host capability. Integration records become
   searchable beside local docs without the plugin touching index files directly.

### Reading the index

The indexed contract exposes these read operations:

| op | arguments | description |
|---|---|---|
| `sources` | *(none)* | Enumerate sources, their entity types, and record counts |
| `search` | `query[, source, entity, limit]` | Ranked keyword search over the index |
| `get` | `source, entity, id` | Fetch one full record by address |
| `list` | `source[, entity, offset, limit]` | Enumerate records from a stable snapshot |
| `relation` | `source, entity, id[, rel]` | Follow a record's typed links |
| `batch_get` | `source, entity, ids` | Fetch several records of one entity |

Call `sources` first when the available sources are unknown; it returns every real source key and
the entities it contains. A Flux-Lang plan can then mix retrieval with other operations:

```flux
hits = search(query: "rate limiting", source: "docs")
ctx evidence
  purpose "answer from the indexed documentation"
  include hits
answer = ai.reason(ask: "How do we rate-limit?", ctx: evidence)
```

These operations are declared read-only and low risk, but the active authorization policy remains
the floor for every dispatch.

### Backends and ranking

The index location is pluggable while retrieval semantics stay the same:

- **In-memory** (the default)—built fresh per run; used by auto-indexing and program declarations.
- **SQLite**—a persistent per-scope index with FTS5 and BM25 keyword ranking.
- **Postgres**—for embedders, behind the `postgres` feature: a shared table namespaced per scope,
  with full-text search. See [storage](../reference/storage.md#datasource-records).

Ranking is keyword/relevance-based by default. With the `embeddings` feature and an embeddings API
key, a semantic layer wraps the keyword backend and embeds records during ingestion.

## Live systems of record

A live datasource leaves data in its source system and implements one async `LiveDatasource`
backend. The host declares its domain schema—entities, filters, default/max page sizes—and flux
generates exactly two operations:

- **`<domain>.list { entity, page?, limit?, filters? }`** returns compact rows and an optional
  `next` cursor.
- **`<domain>.get { entity, id }`** returns a full row or `not found`.

For a domain registered as `support`, the catalog contains `support.list` and `support.get`.

### Validation and paging

Flux validates the static schema when the backend is registered. Before a list call reaches the
backend, the generated operation:

- rejects unknown entities and filter names;
- enforces required filters and their declared string, integer, boolean, or enum types;
- rejects invalid enum values;
- applies the entity's default limit and clamps requests to its maximum.

The cursor is deliberately opaque. Flux validates that `page` has the declared string shape and
passes it through unchanged; the backend that minted it validates its own cursor format and state.
It returns another cursor only when more data exists. Cursors must not contain credentials,
sessions, or connection handles because they can appear in model-visible results and event history.

### Weak rows, exact authority

A live `Row` is plain projection data: stable `id`, `title`, `summary`, and optionally a weak
`Reference`. A reference is either another `(entity, id)` locator or a non-secret navigation URL.
It is never a token, credential, presigned secret URL, database handle, session, or live connection.
`<domain>.get` re-enters the host-owned backend by id, where authentication and connection state are
resolved again outside the model.

Every invocation requires `datasource.read` for the exact `<domain>/<entity>` resource. The backend
also declares its concrete external access:

- a network subject adds exact `network.fetch` authority;
- a connection target adds exact `connection.dial` authority;
- an in-process backend declares neither.

Filter values, cursors, and ids do not become permission subjects. Planning and dispatch consume
the same typed requirements, and denial happens before the backend executes. The backend must still
perform real IO through flux's guarded host facilities.

### Honest catalog surfacing

Live operations are evidence-gated per domain. SDK registration with
`ClientBuilder::try_with_live_datasource` installs the two operations, their domain group, and a
configured-domain ambient signal together. The model therefore sees `support.list` and
`support.get` only when a support backend is actually present. `FLUX_SURFACE_ALL` can reveal the
catalog for debugging, but it never grants authority or bypasses dispatch.

The hermetic SDK example implements tickets and customers with typed filters, backend-owned
cursors, get/not-found behavior, and real executor dispatch:

- [`examples/live_datasource.rs`](https://github.com/codewandler/flux/blob/main/crates/flux-sdk/examples/live_datasource.rs)
- [`examples/support/live_datasource.rs`](https://github.com/codewandler/flux/blob/main/crates/flux-sdk/examples/support/live_datasource.rs)

```bash
cargo run -p codewandler-flux-sdk --example live_datasource
```

For embedding code and the indexed `try_register_pack` recipe, see
[SDK datasources](../sdk/datasources.md).

## Work boards

A **work board** is not a datasource — it mutates, and a datasource is read-only by definition. It
is a write-capable work registry with a
typed item state machine—`ready`, `claimed`, `in_progress`, `review`, `done`, `blocked`,
`failed`—behind a swappable backend. The full spine, and which transitions are legal, is in
[Boards](../coding/boards.md#profile-general-planning-or-execution). Indexed knowledge and live read domains
cannot express work that is claimed, moved, retried, and commented on, which is what a coordinator
agent needs in order to hand tasks out and reconcile them after a crash.

Flux-Lang gives boards their own declaration and registry:

```flux
board product
  scope "repository"
  profile "execution"
  kind "markdown"
  root "./board"
```

The declaration's **name** becomes the operation prefix. A binding named `board` generates
`board.list`, `board.get`, `board.create`, `board.transition`, `board.claim`, `board.comment`,
`board.record_dispatch`, `board.query`, `board.comments`, `board.reassign`, and
`board.record_evidence`. A knowledge datasource is never promoted to a board, and a board never
enters the datasource catalogue. The retired `datasource ... kind "board:*"` spelling fails with an
exact first-class replacement instead of opening a second registry.

Available backends:

| Kind | Storage | Use |
|---|---|---|
| `markdown` | one markdown file per item under `root`, with a derived index | durable — survives a restart, so a coordinator can re-derive its runs |
| `memory` | in-process | a single run, and tests |

`root` is resolved relative to the **program file's** directory, and the board inherits the
session's guarded filesystem root rather than opening one of its own.

`memory` cannot outlive the process that created it, so a Program relying on crash recovery wants
`markdown`. Later Jira/Trello adapters attach to the same board registry without becoming
datasource kinds.

The machine-oriented reads are `board.query`, which returns a page as typed JSON rows (every field
present, absent optionals as `null`) so a flow can `each` over items and `match` on their state, and
`board.comments`, which returns one item's notes as an array. `board.query` also accepts a
`depends_on` filter that keeps only items whose dependencies are all `done`. `board.list` and
`board.get` render prose for reading. See
[Boards](../coding/boards.md#the-stable-agent-api).

The seven mutating operations are gated like any other write: each reports a concrete
`board:<name>/item/<id>` approval subject—`board:<name>/item/new` for `create`, since no id exists yet—so a grant
scoped to one item can never move another. `transition` validates the edge against the state machine
*before* writing, so an illegal move is a clean error and leaves the item byte-identical.

## Related docs

- [Operations](../language/ops.md)—the catalog both datasource forms and work boards use.
- [Endpoints](./endpoints.md)—discover and consume live service connections as weak references.
- [Multi-agent programs](./programs.md)—declare indexed knowledge in a program file.
- [Plugin authoring](../plugins/authoring.md)—contribute records from an integration.
- [Storage](../reference/storage.md#datasource-records)—persist indexed records.
- [Concepts](../concepts.md)—the mental model behind operations, symbols, and the safety envelope.
