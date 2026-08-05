# Design — Batched inputs for repeatable observation operations

**Status:** proposed · **Epic:**
[C-584](../stories/C-584-batched-operation-inputs-epic.md) · **Stories:**
[C-585](../stories/C-585-git-diff-accepts-multiple-paths.md),
[C-586](../stories/C-586-mine-operation-history-for-batchable-inputs.md)

## Why

An agent often asks the same read-only operation about several independent inputs. If the operation
accepts only one input, the model must emit several calls, Flux must authorize/dispatch/render each
call, and the provider history retains each call/result envelope. C-528 overlaps independent calls
from one provider response, but concurrency does not remove their call count, schema overhead or
transcript growth.

`git_diff` is a concrete example. Its optional `path` accepts one string even though one guarded
`git diff -- path-a path-b ...` invocation has the desired semantics. A structural census of the
local Flux event store on 2026-08-05—operation names and event ordering only, no argument or prompt
contents—found 84 immediately repeated `git_diff` calls and 98 excess same-turn `git_diff` calls
across 30 turns. That is enough evidence for the first story and for a broader measured audit.

## Contract

### Preserve one operation's semantics

Batching belongs inside an operation only when several inputs can share one invocation without
changing authorization, approval, ordering or failure meaning. Suitable first candidates are
bounded, observational and naturally return one ordered/labelled result. Each batched call must:

- retain the singular input form for compatibility;
- declare a bounded non-empty array form and reject malformed, mixed or over-limit input before IO;
- derive one exact permission subject per input rather than replacing them with a wildcard;
- preserve stable input/result correlation and label partial/unavailable results honestly;
- pass through the same `Executor::dispatch`, guarded IO and output limits as the singular call; and
- prove the batched result is observationally equivalent to the corresponding singular calls where
  the underlying command/API offers that equivalence.

Do not batch mutations merely because they repeat. Writes need separately designed atomicity,
approval, rollback and partial-failure contracts. Do not add a universal array wrapper around every
operation: operation owners know whether one backend request, one process argv or an explicitly
bounded internal loop preserves semantics.

### `git_diff` first

`git_diff.path` becomes `string | string[]`. The string remains byte-for-byte compatible; the array
is non-empty and bounded by count plus encoded argument bytes. Flux constructs one fixed argv:

```text
git diff --no-ext-diff --no-textconv [--staged] -- PATH...
```

The explicit `--` remains the pathspec boundary. Permission subjects contain every normalized input
path. Omitting `path` still means the whole working-tree/index diff. Filenames beginning with `-`,
spaces, Unicode and pathspec metacharacters are tested without shell parsing. One combined Git diff
is the canonical result; Flux does not concatenate independently truncated child results.

### Mine history without collecting user content

C-586 reads an explicitly selected Flux SQLite event store in read-only mode and emits aggregate
structural findings only:

- operation name and live input-schema digest;
- same-turn and immediately adjacent repetition counts;
- whether the schema already accepts an array or a batch companion already exists;
- access/effect/risk/idempotency metadata and result-size/failure signals; and
- a recommendation: schema guidance, array candidate, concurrency-only, composite operation, or no
  batching because semantics differ.

The audit never exports prompts, arguments, paths, results, subjects, secrets or session text. Any
temporary working set stays local and the committed report contains only aggregates with minimum
sample thresholds. The audit files separately contracted follow-up stories only for evidenced,
reviewed candidates; it does not mechanically rewrite schemas.

## Relationship to existing work

- C-528 schedules independent provider-emitted calls concurrently; this epic reduces calls when one
  operation can honestly consume several inputs.
- `read` already accepts a string or array and `read_many` exists. Repeated `read` calls may therefore
  indicate surfacing/guidance behavior, not another schema change.
- Resource accounting C-574 can later quantify tokens, calls and wall time saved, but does not gate
  the semantic fix.
