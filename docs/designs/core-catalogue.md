# Versioned core catalogue

## Decision

`flux catalog core --format json` is the authoritative offline projection of core catalogue data.
It assembles the ordinary tool registry, selects a deliberately small foundational operation set,
and joins that with `flux_lang::schema` output. It performs no operation and touches no network.

The schema-version-1 document contains `operations`, `nodes`, `capabilities`, and the three JSON
Schemas needed to validate the bundle, individual entries, and the strict AST projection. Ordering
is canonical: operations by runtime name, nodes in language declaration order, and capabilities by
canonical `$id`.

## Identity

Every entry's `$id` is the HTTPS JSON resource that downstream publication must serve. Runtime names
are separate. The stable path categories are network/application, network/transport,
network/internet, data/transform, and language/node. A versioned `$id` can evolve compatibly; a
breaking wire or semantic identity change requires `/v2/`.

The published AST schema is `flux_lang::schema::ast_schema()` with a stable `$id` and an added
`$anchor: node-<kind>` on every `Node.oneOf` branch. This is a projection only: it does not modify
the AST's serde format or maintain another node list.

## Inventory

The callable inventory is `http.request`, `compare`, `map`, `filter`, `dedupe`, `sort`, `flatten`,
`skip`, `top`, `merge`, `join`, `split`, `len`, `first`, `last`, `sum`, `count_by`, `group_by`,
`any`, `all`, `has`, `pick`, `omit`, `merge_obj`, `coalesce`, `keys`, `values`, `regex_match`, and
`regex_extract`.

HTTP is an available capability linked to its operation. DNS, TCP, UDP, and ICMP are planned and
non-callable. `noop` is not a tool: an empty body or `return null` carries that language meaning.

## Compatibility

The wire structs and their `schemars` output are the schema authority. The CLI writes one pretty
JSON document plus one newline to stdout. Downstream repositories vendor the bytes; they do not link
against the CLI or depend on a sibling checkout.

