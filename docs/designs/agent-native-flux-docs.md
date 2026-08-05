# Design — Agent-native Flux documentation

**Status:** proposed · **Epic:**
[C-578](../stories/C-578-agent-native-flux-documentation-epic.md) · **Stories:**
[C-579](../stories/C-579-build-the-release-matched-flux-docs-datasource.md),
[C-580](../stories/C-580-expose-bounded-flux-docs-retrieval-operations.md),
[C-581](../stories/C-581-surface-flux-docs-for-self-directed-questions.md)

## Why

`flux docs` already serves the documentation bundled with the running binary, and generated Flux
skills describe the CLI, language and operation catalogues. A running agent still cannot ask the
same release-matched corpus a bounded question. It must already know a page, read an arbitrary
checkout path, use network search, or answer from model memory. That is especially poor for Flux's
own evolving concepts: Agent-Loop, Flux-Lang, Board/Fleet, permissions, SDK assembly and release
behavior.

This is a datasource problem. Flux documentation is a named, declared, read-only record surface:
operations do not mutate it, records have stable identities and search/get/list/relation are the
right vocabulary. Board is not a datasource because it owns a write-capable state machine and
pluggable tracker mutations; documentation satisfies Decision 0006's datasource definition exactly.

## Contract

### One release-matched indexed datasource

Every complete Flux agent assembly registers the compiled-in indexed datasource `flux-docs`. Its
content is derived deterministically from the same public source and generated references used to
build `crates/flux-server/assets/public-docs.zip`; it never reads an arbitrary checkout at runtime.
The datasource records its Flux version and corpus digest, so an answer can state which release it
describes.

The v1 record model is intentionally small:

| Entity | Identity | Purpose |
|---|---|---|
| `topic` | stable slug such as `agent-loop`, `flux-lang`, `board-fleet` | curated bounded overview, aliases and ordered starting points |
| `page` | canonical public-doc path | page title, description and normalized release-matched body |
| `section` | page path plus stable heading anchor | focused text for bounded retrieval and search hits |

Typed relations connect topics to pages, pages to sections and related topics to one another. Topic
records are authored navigation metadata, not model-generated summaries. A topic overview is
therefore an ordinary bounded `get` of a `topic` record, optionally followed through relations; it
cannot hallucinate a second documentation truth.

The initial topic catalogue covers at least Agent-Loop, Flux-Lang, Board/Fleet, operations and
tools, permissions and approvals, configuration, providers, SDK, plugins/connectors, datasources,
sessions/durability and troubleshooting. Aliases such as `outer loop`, `fleet`, `flow`, and command
names resolve to the canonical topic without duplicating content.

### Reuse the datasource operations

The agent uses the indexed datasource contract already owned by Flux:

- `sources` discovers `flux-docs` and its entities;
- `search { source: "flux-docs", ... }` returns ranked, bounded topic/page/section records;
- `get` retrieves one exact topic, page or section;
- `list`, `relation` and `batch_get` support navigation and multi-page grounding.

There is no parallel documentation search engine and no operation that opens arbitrary paths or
URLs. A concise `flux_docs_overview(topic)` convenience operation may project the exact topic record
and its ordered relations, but it must delegate to the registered datasource and return the same
record identities and corpus metadata. It is sugar, not another retrieval contract.

Results have explicit limits for hits, per-record text and total returned bytes. Truncation is
typed and includes continuation identities. Search ranking, normalization and topic resolution are
deterministic and work offline. The ordinary datasource subject grammar applies, for example
`datasource:flux-docs/topic/agent-loop`; operations declare read-only datasource access and pass
through `Executor::dispatch` like every other agent-visible operation.

### Intent-aware surfacing

The datasource is registered in every complete agent assembly, but its retrieval vocabulary is
surfaced when the request is clearly about Flux itself or working with Flux. That includes Flux
commands and configuration, Agent-Loop, Flux-Lang, Board/Fleet, the SDK, providers, safety,
permissions, plugins/connectors, datasources and troubleshooting.

Activation uses the normal intent/tool-group selection contract and topic aliases, not an isolated
substring switch. A mention of `flux` as an unrelated word must not inflate an unrelated turn. An
explicit request to consult Flux documentation or an explicit group selection always activates it.
The selected group tells the model that `flux-docs` is the authoritative release-matched source and
shows the shortest overview/search/get call shapes; it does not inject the corpus into the prompt.

If the question concerns behavior newer than the bundled release, the operation reports its release
identity rather than silently using the network. External/current research remains an explicit
separate capability.

### Human and agent discovery

The generated Flux skill and public agent documentation explain that Flux questions should consult
the `flux-docs` datasource. A small CLI projection may expose the same typed results for inspection
and tests, but `flux docs` remains the browser/server surface and the datasource remains the agent
surface. Neither shells out to the other.

## Non-goals

- Indexing arbitrary repository design notes, user files or the open web into the built-in source.
- Replacing generated language/operation schemas with prose search.
- Allowing an agent to register or mutate documentation at runtime.
- Treating Board as a datasource again.
- Generating free-form topic summaries at lookup time.

## Delivery order

1. C-579 builds and release-gates the deterministic datasource artifact and topic catalogue.
2. C-580 registers it through the indexed datasource seam and proves bounded offline retrieval.
3. C-581 adds intent-aware surfacing, generated-skill guidance and public operator documentation.

