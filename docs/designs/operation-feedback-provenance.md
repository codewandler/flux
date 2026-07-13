# Operation feedback provenance

## Failure

Flux-Lang retained every gathered result but rendered it as `[read]\n<view>`. In a plan containing
several reads, the next planner request received the values and line numbers without the resolved
file paths. A live matched-effort test consequently produced correct calculations with invented
citations (`data/plans.md`, then `handbook/service-plans.md`). The highest-effort sample happened to
remember the path, but the runtime contract made correctness depend on model memory.

## Contract

`run_call` already owns the resolved, named JSON input and the runtime already has a bounded
`op_summary_prefix` used for symbol summaries. It returns that safe prefix alongside the call outcome.
Transcript writers use it in their labels:

```text
[$plans = read handbook/plans.md]
...

[grep "Northwind" in data]
...
```

Only the existing `read` and `grep` summaries are eligible. Flux does not dump arbitrary operation
arguments, which could duplicate large prompts or secret-bearing values. The result body, stored
canonical value, sink output, replay cell, and safety-envelope dispatch remain byte-for-byte on their
existing paths; this changes only the model feedback label.

## Verification

The runtime regression executes two independent reads through both the optimized physical-plan path
and the sequential interpreter and asserts that their transcript labels remain distinct. All 346
`flux-lang` unit tests and its integration/doc tests pass.

A fresh live run in `/tmp/flux-adhoc-e2e-20260713` repeated the same support-operations question that
previously invented `data/plans.md` and `handbook/service-plans.md`. The exact outgoing feedback now
contained the bounded search label, and the medium-effort answer cited all four real files correctly
in 21.8 seconds. A second low-effort run forced the four source files and also returned the correct
answer and citations in 14.4 seconds. These are targeted live proofs, not a broad quality claim.
