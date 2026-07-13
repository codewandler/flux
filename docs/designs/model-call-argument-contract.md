# Model call-argument contract

## Failure

Four fresh Codex runs all emitted `grep` as four positional literal nodes even though the planner
prompt says a multi-parameter operation takes one named object. Every first plan was rejected, adding
a 6–10 second, roughly 17k-input-token repair request. The model-facing JSON schema exposed only
`args: Node[]` with no field description, while the rejection illustrated a plain JSON object rather
than the Flux AST node the model needed to construct.

## Boundary

Flux keeps named arguments authoritative. Optional parameter names are a set sorted for stable
display, not a positional ABI, so the compiler must not silently guess whether a string argument was
intended as (for example) `grep.glob` or `grep.path`.

The immediate repair is schema-adjacent and diagnostic-driven:

- document on `Node::Call.args` that multi-parameter calls contain exactly one `obj`/object-valued
  `lit` node;
- render the valid AST envelope in positional-call errors;
- append the live `OpSignature`'s required/optional names and types.

This does not add more global prompt prose, change the serialized AST, or weaken analysis. A matched
live rerun decides whether this closes the repeated failure. If not, the model emission format needs
a measured redesign; another prose-only hint is not sufficient.

## Verification

The schema and analyzer regressions failed before the change: `Call.args` had no description, and
the error showed only a plain `{"path": …}` tool-input object. They now prove that the merged schema
carries the convention and that rejection includes both the actual Flux AST envelope and the live
operation signature.

Live results on the identical support-operations prompt:

- before: four of four low/medium/high runs emitted invalid positional `grep` arguments and paid a
  6–10 second, approximately 17k-input-token repair request;
- after: three of three fresh runs (two low, one medium) emitted valid named objects immediately;
- the low runs completed correctly with two model calls in 13.2 and 14.4 seconds;
- the medium run made no repair, but chose an additional useful evidence round and completed in 26.6
  seconds—proof that this removes one source of waste, not all provider/model latency.
