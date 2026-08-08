---
id: C-592
title: "Search every RFC from a fast local indexed datasource"
pillar: Core
status: backlog
epic: datasource-rag
design: docs/designs/datasource-rag.md
areas: [flux-cli, flux-capabilities, flux-datasource]
note: "mirror the RFC Editor corpus into a versioned local cache; search/get/list are offline and never crawl one RFC per query"
---

# Search the RFC Editor corpus locally

## Goal

Expose the RFC Editor's complete published RFC corpus as an indexed `rfc-editor` datasource. Fetch
and refresh the corpus into a durable local cache once, then make title/body/metadata search and
exact RFC lookup fast and network-independent.

## Acceptance

- [ ] Failing first, a hermetic RFC-index fixture with gaps, obsolete RFCs, updates and more than one
      available format proves that numeric-range crawling misses or misclassifies the corpus. The
      ingester enumerates the RFC Editor's published index/mirror inventory and gives every listed
      RFC a stable `rfc.document` identity derived from its RFC number.
- [ ] Records carry the RFC number, title, authors, publication date, stream/category/status,
      current/obsoleted-by/updates relations, canonical `https://www.rfc-editor.org/rfc/rfcNNNN.*`
      provenance and searchable text chunks with deterministic ids. Missing optional formats do not
      drop an otherwise published RFC.
- [ ] The indexed source is named `rfc-editor`; `sources`, `search`, `get`, `list`, `relation` and
      `batch_get` use the existing indexed-datasource contract. Search spans every cached RFC by
      title, number and body, while exact lookup such as RFC 9110 returns bounded records and the
      canonical source URL rather than refetching the document.
- [ ] The initial sync uses only the RFC Editor's published bulk/index surfaces and exact
      `rfc-editor.org` network authority. It is bounded and cancellable, limits document/total cache
      size, uses atomic snapshot publication and leaves the previous complete cache usable after a
      failed or interrupted refresh.
- [ ] The cache lives under Flux's platform cache root (with an explicit test/operator override),
      never in the workspace or event store. A versioned manifest records corpus/index validators,
      fetch time, schema/chunker version, document counts and content digests without storing
      credentials or response headers wholesale.
- [ ] Refresh performs incremental validation/download, incorporates new or changed RFCs, removes
      entries no longer present in the selected official mirror inventory and reindexes only changed
      content. An unchanged refresh transfers no RFC bodies and produces a byte-identical searchable
      snapshot.
- [ ] Once one snapshot exists, datasource registration and every retrieval operation work with the
      network disabled and perform zero remote reads. Cache refresh is an explicit host/operator
      lifecycle action, not a model-facing datasource mutation and not an implicit delay on each
      agent turn.
- [ ] RFC text is treated as untrusted retrieved knowledge: shared containment/render bounds apply,
      snippets and full-record reads are capped, redirects cannot leave the admitted RFC Editor
      origin and malformed index/document content fails closed without poisoning the active cache.
- [ ] Hermetic fixtures cover first sync, offline reopen, unchanged and incremental refresh,
      corruption recovery, index gaps, an RFC without text format, relation traversal and ranked
      cross-corpus search. Public datasource/cache documentation, full repository gates and embedded
      documentation freshness pass.

## Progress

- 2026-08-06 — filed from operator request. The RFC Editor publishes a bulk download contract and
  recommends its `rfcs-text-only` rsync module for a local up-to-date mirror; it also publishes an
  RFC index and per-RFC HTTPS resources. The implementation must use those inventories rather than
  probing every integer below the newest RFC.

## Notes

- This is indexed mode under roadmap Decision 0006: the local copy is the product feature, not a
  live datasource that performs thousands of remote reads during search.
- The official bulk-download instructions are at
  <https://www.rfc-editor.org/series/rfc-download/>. Transport selection must preserve the same
  bounded, incremental, exact-origin contract on platforms without an `rsync` binary.
- Errata are a separate RFC Editor surface and are not silently folded into canonical RFC text in
  this story. A follow-up may index errata as linked records without rewriting the RFC record.
