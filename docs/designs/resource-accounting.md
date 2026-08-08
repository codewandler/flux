# Design — causal resource receipts and result cost

**Status:** proposed · **Epic:**
[C-574](../stories/C-574-resource-accounting-epic.md) · **Stories:**
[C-575](../stories/C-575-causal-resource-usage-receipts.md),
[C-576](../stories/C-576-attribute-resource-usage-to-board-work.md),
[C-577](../stories/C-577-resource-bills-and-rollups.md)

## Outcome

Every produced result can answer: **which measured resources were used to produce this, where were
they used, and what did they realistically cost?** A Fleet story links to an immutable resource bill;
the same ledger groups it by request, story, epic, worker, wave, repository and Fleet. When only token
usage is known, the bill says exactly that. Missing CPU, network or price data stays unsupported or
unpriced rather than appearing as zero.

This is accounting, not budgeting. C-542/C-571 use the measurements to enforce targets and limits;
C-573 uses freshness-labelled projections to adapt allowed Fleet policy; C-518…C-524 visualize
historical usage. All three consume the same receipts.

## Existing facts to preserve

- `CallUsage` is the canonical Flux per-model-call token record; legacy `TurnEnded.usage` fills only
  uncovered old turns. Summing both would double-count.
- Provider-reported cost wins for that call. Pricing-table or subscription-equivalent values are
  labelled estimates, and an unknown price is never `$0`.
- Sub-agent usage already has an explicit parent correlation and a fixed rollup rule; the bill must
  not add child-inclusive totals to the same child rows again.
- C-519/C-520 own the shared cross-harness model-usage timeline and truthful provider/model/cost
  normalization. Native resource spans enrich it; they do not replace its foreign-harness readers.
- Board state is repository-owned. Resource accounting links immutable evidence to a BoardRef; it
  does not hand-maintain totals in story prose or make Board a datasource.

## The receipt

One append-only ledger stores small typed spans. Exact names may change, but the versioned contract
has this shape:

```text
ResourceUsageReceipt
  receipt id + schema version
  root request/result id
  span id + parent span id
  agent/session/worker/wave/repository
  optional BoardRef + assignment revision
  loop binding + phase + operation/backend
  start/end + clock precision
  measurements[]
  monetary charges[]
  sources + freshness + coverage
  correction-of (optional)
```

Measurements are additive counters or durations with explicit units and source. The first catalogue
includes:

| Family | Measurements | Honest source |
|---|---|---|
| Model | calls, fresh/cache input, output, reasoning/audio/other tokens | provider response / harness record |
| Runtime | wall time, loop iterations, tool dispatches, reports, retries/rework | host monotonic clock/counters |
| Process | user/system CPU time, peak RSS when available, exit/output bytes | owned child/container/OS accounting |
| Network | DNS/connect/TLS/TTFB/transfer time, requests, bytes in/out | instrumented guarded transport |
| Filesystem/artifact | bytes read/written, artifact/diff/output bytes when measured | guarded tool/backend |
| Capacity | live-agent/tool concurrency occupancy and queue time | host census/semaphore |
| Validation | targeted/review/gate command wall/CPU/output | host-owned process runner |

An in-process library cannot promise process RSS or CPU attribution it does not own. A remote or
foreign backend reports only dimensions its conformance contract can measure. Every absent dimension
is `unsupported`, `not_reported` or `not_attributable`, never numeric zero.

Receipts contain counts, timings, ids, bounded labels and digests—never prompts, answers, tool
arguments, command output, URLs with secrets, file content or network payloads.

## Monetary value

Charges are separate from physical measurements:

```text
MoneyCharge
  amount + currency
  basis: provider_reported | pricing_table | subscription_equivalent | operator_rate
  rate/version/effective-at
  coverage: complete | partial | unpriced
  source receipt ids
```

Tokens remain visible even when no price exists. CPU, network, storage and process time become money
only when an operator supplies a versioned rate or a backend reports a real charge. A cloud invoice
estimate and local CPU seconds are not silently combined into “actual cost”; the bill shows physical
usage, reported charges and estimates in separate columns plus a coverage statement.

Historical repricing creates a new projection or adjustment naming the pricing basis. It never edits
the original measured receipt or implies that a current price was billed in the past.

## Causal attribution

Time-window coincidence is insufficient: five workers and the coordinator run concurrently. Every
span inherits an explicit root request/result id and parent span. A Fleet admission also binds its
BoardRef, assignment revision, worker and wave, so writer, nested task, reviewer, rework, targeted
checks and handoff spans remain causally attached to the story.

Three totals are distinct:

- **exclusive/direct** — measurements recorded by spans directly owned by the selected scope;
- **inclusive** — direct plus causal descendants, each receipt counted once; and
- **allocated** — inclusive plus a declared share of otherwise shared overhead.

Coordinator planning, integration gates and idle Fleet supervision may serve several stories. They
remain `shared/unallocated` by default. An optional versioned allocation policy may divide them
equally, by direct cost, by runtime or another stated basis. The output always shows the allocated
amount and policy separately; it never hides overhead inside “direct story cost.”

Board epic aggregation uses the Board snapshot/revision that defined membership for the report.
Changing an epic later does not rewrite old bills. Corrections append an adjustment referencing the
original receipt; durable history stays auditable.

## Storage and Board linkage

The canonical ledger lives with durable runtime/Fleet evidence and is indexed by causal and Board
identities. A story receives a bounded `record_evidence` link containing receipt id, digest, coverage
and compact direct/inclusive totals. Generated Board Markdown may project that summary, but story
frontmatter is never the accounting database.

The linkage happens only after the host verifies the assignment/result identity. A worker cannot
attach arbitrary spend to another story or erase its own usage. Board backend adapters—Track, Jira,
Trello or later systems—store the common evidence reference through their normal backend contract.

## Queries and rollups

One projection powers CLI, JSON, Fleet status and later TUI views. It can group or filter by:

- root request/result and task;
- BoardRef/story and epic at a selected Board revision;
- worker/agent/session/model/provider/backend;
- wave/repository/Fleet;
- loop binding/phase and validation/review/rework; and
- time range.

Every result includes measurement coverage, cost provenance and exclusive/inclusive/allocated
columns. Large histories use indexed/bounded reads and return omission metadata rather than loading
transcripts.

## Delivery order

1. **C-575** — record immutable causal resource spans and measurements.
2. **C-576** — bind spans to requests/results/BoardRefs and define non-double-counting rollups plus
   shared-overhead allocation.
3. **C-577** — expose bills/evidence links and story/epic/worker/wave/Fleet queries.
4. **C-571/C-573** — consume the same ledger for hierarchical budgets and adaptive Fleet policy.
5. **C-519/C-520 and C-518 TUI** — merge native resource receipts with existing cross-harness
   model-usage facts while retaining truthful partial coverage.

## Non-goals

- Invoice reconciliation or claiming estimates are billed truth.
- Persisting prompt, answer, command, file or network content for accounting.
- Guessing provider, ownership, price, CPU or network usage from a model name or wall-clock overlap.
- Making Board a metrics store or datasource.
- Letting a worker edit its bill, another story's attribution or the Fleet's allocation policy.
