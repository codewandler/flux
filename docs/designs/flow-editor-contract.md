# Design: a host-neutral flow-editor projection

## Decision

The editor is a projection of `DraftAst`, not another workflow runtime. `flux-lang` owns a versioned,
structured `EditorFlow` plus pure `project`/`lower` functions. Hosts own storage, layout, tenancy and
HTTP. The interpreter continues to execute the lowered `DraftAst` through `OpHost::dispatch`.

The visual v1 subset is deliberately structured: calls/binds, `when`, bounded `repeat`/`each`,
`parallel`, and `return`. Arguments and conditions remain ordinary Flux expression nodes. This makes
arbitrary cycles unrepresentable; loops stay explicit bounded language constructs. A valid node the
projection does not understand returns a source-only diagnostic and no editable graph.

## Identity and source

Each projected statement has an opaque editor id. A projection may take the preceding projection and
reconcile ids by exact path plus semantic hash, then by semantic hash for a moved statement, then by
path for a statement edited in place; unmatched nodes receive new ids. This order prevents a deletion
from transferring its id to the unchanged statement that shifts into its path. The lowered AST carries
no editor-only field. The graph derives its `NodeMap` from its current structure rather than trusting
projection-time paths, so graph reordering preserves ids while changing runtime addresses. Exact
author source remains a host concern: lowering produces canonical Flux, while a host presented with
unsupported syntax or trivia keeps the original source and withholds graph mode.

## Inspection and execution

Call inspectors read the existing `OpCatalog`/JSON schemas. Static control-node fields come from the
typed editor variants. Diagnostics use existing analyzer diagnostics plus projection diagnostics and
source ranges.

The interpreter accepts an optional node map/trace observer. It emits structural node enter/success/
failure and branch-selection records identified by node id and occurrence. These records contain no
arguments or results; ordinary guarded operation events remain the audit/value trail. Existing callers
that provide no map behave byte-for-byte as before.

## Boundaries

- No Vue, coordinates, tenancy, persistence or HTTP types enter Flux.
- No operation becomes callable because it appears in a graph; the host-supplied catalogue and
  authorization envelope remain authoritative.
- A source-only result is success with an explicit limitation, not a parse failure and not a lossy
  graph.
