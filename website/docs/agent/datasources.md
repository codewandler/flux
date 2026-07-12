---
title: Datasources
description: "The agent's indexed knowledge layer: records, how knowledge gets in, and the retrieval operations that read it."
---

# Datasources

A **datasource** is the agent's knowledge layer: an indexed store of **records** the agent can
search and read, instead of re-reading raw files or being handed a giant prompt. If
[operations](../language/ops.md) are what the agent can *do*, a datasource is what the agent can
*look up*.

## Datasources vs. operations

The two concepts sit at different layers, and the relationship is deliberately simple:

- An **operation** is the universal callable unit — the verbs of the system. Every tool, plugin
  operation, toolchain command, and cognition op is an operation in one catalog, and every call
  crosses the same [safety envelope](./safety.md) (authorization → approval → guarded IO).
- A **datasource** is one specific capability: a queryable index of knowledge records. It is a
  *noun* — it does nothing on its own.
- The agent reaches a datasource **through operations**. Retrieval is exposed as ordinary read-only
  ops (`sources`, `search`, `get`, `list`, `relation`, `batch_get`) registered in the same catalog as every
  other operation. There is no side door: reading knowledge is a governed call like any other.

So "datasource vs. operation" is not a choice between alternatives — a datasource is *served by*
operations. Plugins illustrate both sides at once: a plugin projects new operations (verbs) into
the catalog **and** can contribute records (knowledge) into the datasource.

:::note Datasources are not endpoints
A datasource is indexed knowledge. An [endpoint](./endpoints.md) is a weak reference to a live
service passed to an operation such as `sql.query`. Registering a database endpoint does not turn
its rows into datasource records; that live, paged bridge is a separate capability.
:::

## Records

A datasource holds records, not files. Each record is addressed by `(source, entity, id)`:

- **`source`** — where it came from: `local` for workspace docs, a declared datasource name, or an
  integration such as `gitlab`.
- **`entity`** — the record's type, e.g. `file.document`, `openapi.operation`,
  `gitlab.merge_request`.
- **`id`** — stable within its `(source, entity)`.

A record carries a short `title`, the indexed `body` text, freeform `meta` (url, path,
`updated_at`, …), and typed `links` to other records — so retrieval can follow relations
("the merge requests linked from this issue") instead of only matching keywords.

## How knowledge gets in

Three routes feed the index:

1. **Workspace auto-index.** The CLI agent walks the workspace at startup and indexes
   documentation files (`.md`, `.txt`, `.rst`, `.adoc`, `.mdx`; capped in count and size) under
   the `local` source as `file.document` records. Ambient project knowledge is searchable with no
   setup.
2. **Program declarations.** A [multi-agent program](./programs.md) declares its knowledge
   explicitly. Declared datasources are ingested when the program loads:

   ```flux
   datasource docs
     kind "markdown"     // a directory of docs — or "openapi" for an API spec file
     path "./docs"
   ```

   A relative `path` resolves against the **program file's own directory**, not the directory you
   launched from — so `flux app run /any/where/app.flux` finds the `./docs` shipped beside `app.flux`
   from any working directory. An absolute path is used as-is.

3. **Plugin records.** A [plugin](../plugins/authoring.md) declares datasources in its manifest
   and emits records through the gated `datasource.*` host capability — integration records
   (issues, merge requests, …) become searchable knowledge alongside local docs, without the
   plugin ever touching the index files directly.

## Reading it: the retrieval operations

Retrieval is six read-only operations — low risk, never pausing for approval:

| op | arguments | description |
|---|---|---|
| `sources` | *(none)* | Enumerate the sources in the index: per source, its entity types and record count |
| `search` | `query[, source, entity, limit]` | Keyword search over the whole index, ranked |
| `get` | `source, entity, id` | Fetch one record in full by its address |
| `list` | `source[, entity, offset, limit]` | Enumerate a source's records, paged |
| `relation` | `source, entity, id[, rel]` | Follow a record's typed links to the linked records |
| `batch_get` | `source, entity, ids` | Fetch several records of one entity in one call |

`search`/`get`/`list`/`relation`/`batch_get` all take a `source` argument — so how does the agent
learn which sources exist in the first place? It calls `sources`: no arguments, and the answer is
every source key currently in the index (`local`, any program-declared name, any contributing
plugin), each with the entity types it holds and how many records. Run it first, then `search`/
`list` with a source you now know is real.

These appear in the same [operation catalog](../language/ops.md) as everything else, so a
Flux-Lang plan can mix retrieval with any other work:

```flux
$hits = search({ query: "rate limiting", source: "docs" })
$answer = ai.reason({ ask: "how do we rate-limit?", ctx: $hits })
```

## Backends and ranking

Where the index lives is pluggable, with identical retrieval semantics:

- **In-memory** (the default) — built fresh per run; what the auto-index and program declarations
  use.
- **SQLite** — a persistent per-scope index file; keyword search via FTS5, ranked by BM25.
- **Postgres** — for embedders, behind the `postgres` feature: one shared table, namespaced per
  scope, with full-text search. See [storage](../reference/storage.md#datasource-records).

Ranking is keyword/relevance-based by default. Built with the `embeddings` feature (and an
embeddings API key), a semantic layer wraps the keyword backend and embeds records as they are
indexed.

## Related docs

- [Operations](../language/ops.md) — the full catalog the retrieval ops live in.
- [Endpoints](./endpoints.md) — discover and consume live service connections as weak references.
- [Multi-agent programs](./programs.md) — declaring `datasource` modules in a program file.
- [Plugin authoring](../plugins/authoring.md) — contributing records from an integration.
- [Storage](../reference/storage.md#datasource-records) — where datasource records persist.
- [Concepts](../concepts.md) — the mental model behind ops, symbols, and the safety envelope.
