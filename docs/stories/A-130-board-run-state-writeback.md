---
id: A-130
title: Board write-back of runner and task_id — make "the board is the run registry" true
pillar: Agent
status: done
priority: 33
epic: fleet-coordinator
design: docs/designs/fleet-coordinator.md
areas: [flux-capabilities, flux-orchestrate]
note: "filed from A-116's implementor report — design §5 says the board IS the run registry, but no op can write the two fields that make it one"
---

# Board write-back of runner and task_id — make "the board is the run registry" true

## Goal
[fleet-coordinator.md §5](../designs/fleet-coordinator.md) claims run state needs no second store
because `fleet.dispatch` writes the worker's `task_id` and `runner` address back onto the board
`Item` — "the board is the run registry", which is what makes crash recovery "restart, sweep,
re-derive".

As both implementors reported, that write path does not exist: A-113 lands `WorkBoard` with
`Item.runner` / `Item.task_id` as *fields* but no op that sets them, and `ItemDraft` does not carry
them. Until this lands, the design's crash-recovery story is a claim, not a property.

## Acceptance
- [x] A board operation that records a dispatch — either a seventh op or an extension of `claim` to
      carry `runner` + `task_id` atomically with the claim. **Decide it in this story and say why**;
      atomicity with `claim` is the argument for the extension, and a distinct op is the argument for
      keeping `claim`'s contract narrow.
- [x] Failing-first test: after `fleet.dispatch`, a fresh reader of the board can recover the
      dispatch — worker address and task id — with no in-memory state whatsoever.
- [x] Failing-first test: crash recovery end-to-end — a new process over the same board re-derives
      every in-flight item and its worker, and the sweep resumes. This is A-117's headline claim, so
      the test belongs here or is shared with it.
- [x] Concrete `permission_subjects` on whatever op results, consistent with A-113's `<domain>/item/<id>`.
- [x] The design doc's §5 is updated to describe the op that actually exists.

## Progress

**Decision (Acceptance 1): a seventh op, `record_dispatch`, not an extension of `claim`.**
The atomicity argument for folding it into `claim` does not survive the ordering: the `task_id` does
not exist until the worker answers the send, so the record is necessarily written *after* the claim
either way. Extending `claim` would buy atomicity of `(assignee, runner, task_id)` with the state
change while leaving the only window that matters — worker accepted, board not yet written — exactly
as wide. It would also make `claim`'s `Idempotency::Conditional` incoherent ("same assignee,
different `task_id`" has no answer). `transition` likewise stays the single edge-checked entry into
the state machine, so `record_dispatch` writes those two fields and moves nothing else.

**What landed**

- `flux-runtime` (L2): the `DispatchLedger` port — `subject()` (sync, for the gating path) +
  `record_dispatch()`. It lives at L2 for the same reason `Spawner` does: `fleet.dispatch` is L3 and
  `WorkBoard` is L5, so neither may name the other, and both already depend on flux-runtime.
- `flux-capabilities` (L5): `WorkBoard::record_dispatch` (a required trait method), the generated
  seventh op `<domain>.record_dispatch` (`Effect::Write`, `Risk::Medium`,
  `Idempotency::Conditional`, subject `<domain>/item/<id>` from the shared `item_subject` helper),
  `BoardLedger` as the adapter, and the `MemoryBoard` implementation.
- `flux-orchestrate` (L3): `FleetDispatchTool::with_ledger` and an optional `item` param. The
  write-back is contractual, not best-effort — see the three decided paths below.
- Design `fleet-coordinator.md` §5 gains "The op that performs the write-back", §2 the method.

**The accepted-but-unrecorded window, decided explicitly**

- `item` named with no ledger wired → refused **before any network call**. Dispatching first and
  discovering the gap afterwards is precisely how an orphan is made.
- Board write fails after the worker accepted → a compensating `tasks/cancel` stops the run nothing
  could sweep; if that also fails the op reports `ORPHANED RUN` with the task id and a manual
  `fleet.cancel` recovery line.
- A worker answering synchronously has no task, so nothing is recorded and `"recorded": false` is
  reported — storing a dead id would send the next sweep after a run that no longer exists.

**Deliberately left open:** recording a dispatch against a `Done` item is not refused. Terminal-state
policy belongs to the sweep's semantics, which this epic has not settled; noted in §5 so it is not
re-litigated as an oversight.

**Follow-up owed:** A-114's `MarkdownBoard` is being written against the six-method trait and will
need a `record_dispatch` impl plus the contract-suite property. The coordinator is sequencing that
as a short follow-up after A-114 merges — not done here.

**Gate, second session (the first died mid-flight, leaving the above as a WIP commit plus an
uncommitted diff).** Picked up in place; nothing was discarded or rewritten. Verified the whole gate
green from a cold `target/`: `cargo build --workspace`, `cargo test --workspace` (no failures),
`cargo clippy --workspace --all-targets -- -D warnings` (clean), `cargo fmt --all` (reformatted
`fleet.rs` + `fleet_board_recovery.rs`; `--check` clean after), `cargo test -p flux-codegate`
(13 passed — the L2 `DispatchLedger` seam is a legal edge, no `layer()` change needed).

**Failing-first, demonstrated rather than asserted.** At the merge base (`6418ef81`) `runner` and
`task_id` are only ever written as `None` at item creation and rendered read-only — `git grep` finds
no `record_dispatch` / `DispatchLedger` / `BoardLedger` anywhere under `crates/`. A throwaway witness
reproducing that world (dispatch with no ledger, then ask the board who is running the item) failed
exactly where the acceptance says it must:

```
assertion `left == right` failed: design §5: the board is the run registry, so it must know the runner
  left: None
 right: Some("http://127.0.0.1:45835")
```

**Design tightened to match the code (Acceptance 5).** §5's opening still claimed `fleet.dispatch`
writes the two fields unconditionally; it now points at the op that does it. Added that
`record_dispatch` is a *required* `WorkBoard` method — a defaulted one would let a backend look
healthy until a restart recovered nothing — and that wiring the ledger is the assembler's job, which
nothing in-tree does yet (`FleetDispatchTool` is exported but never registered; that gap is A-131's).

**Breaking, and deliberately so:** `WorkBoard` is public API of `codewandler-flux-capabilities`, so
the new required method breaks any out-of-tree implementor. `MemoryBoard` is the only in-tree one.

**Rework: a latent A-116 defect fixed here (not an A-130 regression).** `fleet.dispatch` could not be
registered into *any* `ToolRegistry`: it declares `Effect::Process` with access
`[Network, Provider]`, and `authority_requirements_from_declaration` refuses a process effect without
process access. The merge base declares exactly the same thing, so the defect predates this branch;
it surfaced because A-131 tried to wire the ops up. Root cause was a coverage hole — A-116's tests
only ever call `.spec()` and `.execute()`, neither of which runs `authority_requirements`, and
`fleet.*` is not in `try_register_builtins`, so nothing else covered it.

Verdicts, one per op: **`fleet.dispatch` broken** (both the plain and the ledger-wired shape);
**`fleet.status` already registrable** (`[Read, Network]` / `[Network]` — the network effect is
satisfied by network access); **`fleet.cancel` already registrable** (`[Write, Network]` /
`[Network]` — the write effect resolves to an `operation.mutate` requirement because network access
is present). Neither status nor cancel was modified.

Fixed by overriding `Tool::authority_requirements` on `FleetDispatchTool`, following the `TaskTool`
precedent. `Effect::Process` is kept (it is the op-cache generation bump, not OS-process access) and
`AccessKind::Process` is deliberately *not* added (it would derive `process.exec` on a Process
resource named by the worker's URL). The override discriminates the two subject families rather than
iterating the flat subject list — a `DispatchSubjects { worker, item }` split shared with
`permission_subjects`, so the two can never disagree:

- worker origin → `network.fetch` + `model.invoke`;
- board item → `datasource.write`, matching `<domain>.record_dispatch` exactly. This is required, not
  double-gating: `BoardLedger` calls the `WorkBoard` backend directly, so the generated op's own gate
  never runs on the ledger path;
- unnameable endpoint → the conservative wildcard (`network:*`, `provider:*`), never an empty
  requirement list, which would mean the op demands nothing at all.

## Notes
- Filed 2026-07-29 from A-116's handoff, corroborated by A-113's. Both implementors independently
  reported the same gap from opposite sides, which is the strongest signal the design had a hole.
- Depends on A-113. Blocks A-117's crash-recovery Acceptance and A-128's monitor journey.
