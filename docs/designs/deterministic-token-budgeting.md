# Deterministic token budgeting

Status: accepted (C-469, 2026-08-03)

## Context

Flux exposed two public traits named `TokenCounter`, but they described different operations and
neither had production wiring:

- `flux-provider::TokenCounter` asynchronously counted a complete provider `Request`.
- `flux-lang::context_slice::TokenCounter` synchronously counted an individual text candidate.

The only implementation was a Flux-Lang test fixture. The only production `slice_context` call
supplied no counter, no budget, and empty candidate text because that call uses the slicer only for
visibility gating. Actual authored `ctx` payload budgets are explicitly character budgets in the
interpreter. No shipped provider performed exact preflight token counting.

The similarly named traits could not be connected directly without violating the layer map:
`flux-lang` is an IO-free L0 contract and cannot depend on the L1 provider abstraction. A useful exact
counter would also need model-specific ownership, request framing, an async failure policy, and a host
that invokes it before budgeted slicing. The existing traits supplied none of that architecture.

Before removing the published provider trait, the 2026-08-03 compatibility audit checked crates.io
reverse dependencies and GitHub code search. crates.io listed ten dependent versions, all crates from
the Flux workspace; the public code search found no external use of `flux_provider::TokenCounter`.

## Decision

Flux-Lang context slicing deliberately budgets with `estimate_tokens`: Unicode scalar characters
divided by four and rounded up. The function's documentation states that this is the production
policy, not a fallback. `slice_context` no longer accepts an optional counter.

Both unused `TokenCounter` traits are removed. Exact counting can return only as a complete design
with a provider/model owner and a production caller; an optional trait with no such path is not kept
as speculative API.

## Consequences

- Context slicing is deterministic, provider-independent, and honest about its approximation.
- Existing production behavior does not change; no runtime path previously supplied an exact count.
- Direct Rust callers lose the two public traits and the sixth `slice_context` argument. Under the
  repository's pre-1.0 compatibility rule, the next release is a minor bump.
- C-462 may reason about compaction in its native character units. It still needs trustworthy model
  context-window metadata and representative session evidence before changing the trigger.

## Rejected alternative

Implementing Anthropic counting alone would make exactness depend on provider selection and require
network IO for a pure slicing decision. Adapting the async request counter into the synchronous text
counter would either block inside Flux-Lang or move provider knowledge into L0. Both choices violate
the current ownership boundary and still leave every other provider on a different policy.
