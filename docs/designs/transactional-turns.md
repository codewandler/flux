# Transactional turns — a compensating undo for the world, not just the session

Story: [A-91](../stories/A-91-transactional-turns-epic.md) · Pillar: Agent · Status: design

## The gap

The Time Machine (A-45/A-46/C-44) and the Deterministic Agent Lab replay, fork, and diff a *run*.
`flux export` (C-132) renders one. All of them are reads over the event log. **Nothing undoes
effects in the world.** A turn that wrote four files, moved a directory, and pushed a branch leaves
no path back except the user's own memory and `git`.

flux is unusually well placed to fix this, because every effect is already a frozen
[`ActionBatch`](../../crates/flux-flow/src/staged.rs) of *literal* calls — the host, never the
model, constructs each `Action { id, op, input }` after validating it against the live tool schema.
A reverse batch is therefore a mechanical transform of data flux already has, not a re-planning
problem.

## One correction to the story's premise

A-91 says the runtime should "synthesize a reverse-batch **at approval time**". That is not
implementable for the dominant case, and the design says so up front rather than discovering it in
implementation.

The reverse of `write(path, new_bytes)` is `write(path, prior_bytes)`. **`prior_bytes` is not
knowable at approval time** — the file may be changed by an earlier action in the same approved
batch, by a concurrent process, or by the user between approval and execution. Synthesizing the
reverse batch when the approval sheet renders would capture a pre-image that is stale by the time it
matters, which is worse than no undo: it would confidently restore the wrong bytes.

So the design splits the two things the story conflates:

| | When | What it is |
|---|---|---|
| **Declaration** | assembly time, on the op spec | *whether* this op can be compensated, and how |
| **Materialization** | execution time, inside the guarded boundary | the concrete reverse `Action`, with the captured pre-image |

Approval time gets what it actually needs and can actually have: **"this batch contains 2
operations with no declared compensator"** — the policy-visible risk signal the story asks for —
without pretending to know bytes it cannot know yet.

## Design

### 1. The compensator contract, declared on the op

`ToolSpec` already carries `effects: Vec<Effect>` (`flux-spec/src/lib.rs:267`). Add a sibling:

```rust
pub enum Compensation {
    /// Reversing this op needs no captured state: the inverse is a pure function of the input.
    /// e.g. `git_branch_create(name)` → `git_branch_delete(name)`.
    Inverse { op: String },
    /// Reversing needs a pre-image captured immediately before execution.
    /// e.g. `write(path, _)` → capture bytes at `path`, reverse is `write(path, captured)`.
    Snapshot { capture: CaptureKind, op: String },
    /// Nothing executed — no compensation needed (every read-only op).
    NotNeeded,
    /// Declared irreversible. The `why` is shown at approval and by `flux undo`.
    None { why: &'static str },
}
```

`NotNeeded` is the default for `Effect::Read` ops, so read-only tools need no annotation. Every
*mutating* op must declare one of the other three — enforced by a test that walks the built-in
registry and fails on a mutating op with no declaration. That test is the mechanism that stops this
from silently rotting as ops are added.

`None { why }` is a first-class answer, not a failure. `send_external` (the mail is sent), `money`,
and `bash` (arbitrary argv — flux cannot know what it did) are honestly irreversible, and saying so
is more valuable than a compensator that pretends.

### 2. Pre-image capture at the one guarded seam

`Executor::dispatch_outcome` (`flux-runtime/src/lib.rs`) is the single funnel every op passes
through: hooks → authorization policy → permission rules → approval → execute through the guarded
`System`. Capture belongs there, immediately before execution and inside the same guarded boundary —
so the capture read is itself policy-checked and audited, and there is no window between capture and
write.

This mirrors the existing `PreToolHook` shape but is deliberately *not* a hook: hooks are user-
extensible and may be absent or fail; compensation capture is part of the envelope and must run for
every mutating dispatch or the dispatch is not compensable. A capture that fails downgrades the
action to `Compensation::None { why: "pre-image capture failed" }` — recorded, disclosed, never
silently dropped.

### 3. The reverse action is an event, not a side table

Per the event-store canon ("adding a kind of flux fact is one new variant plus one projection arm",
`flux-events/src/kind.rs:32`), materialized compensations land as a new variant:

```rust
/// The reverse of one executed action, materialized at execution time. Folding a turn's
/// compensations in reverse order yields the undo batch.
Compensated {
    action_id: String,
    op: String,          // the op that ran
    reverse: Option<Action>,   // None ⇒ declared or degraded irreversible
    why: Option<String>,       // present iff `reverse` is None
},
```

scoped to the turn via the existing `.in_turn(turn_id)`. **Breaking**: `EventKind` is a deliberately
closed set with no `#[non_exhaustive]`, so a new variant breaks exhaustive matches in dependent
crates ⇒ next release is a MINOR per the pre-1.0 rule. This is the sanctioned cost the enum's own
doc comment describes.

Storing the reverse action (rather than recomputing it at undo time) is what makes undo work after a
restart, from a different process, and on a session the current process never ran.

### 4. `flux undo --turn <n>`

A new CLI verb beside the Time Machine's `replay` / `fork` / `diff` / `export`:

1. Load the turn's `Compensated` events (kind-filtered read, same shape as `load_turn`).
2. Build an `ActionBatch` from the `reverse` actions **in reverse execution order** (LIFO — the last
   write is undone first, so a file written twice in one turn lands on the pre-image of the *first*
   write, which is the state before the turn).
3. Execute that batch **through the ordinary approval + guarded-IO envelope**. Undo is not
   privileged: restoring a file is a write, and it is approved and audited like any other. This also
   means the undo itself records its own compensations — `flux undo` is undoable.
4. Report explicitly: how many actions were reversed, and — itemized — which were not, with each
   `why`. A turn that sent an email and wrote a file reports the file restored and the email
   **not** reversed, naming it.

### 5. Ordering and partial failure

Undo executes sequentially in LIFO order and **stops at the first failed compensator**, reporting
what was and was not applied. It does not continue past a failure and it does not roll its own
partial work back.

The reasoning: continuing past a failure produces an interleaved half-state nobody can reason about
(action 5 reversed, 4 failed, 3 reversed), and auto-rolling-back a failed undo is a recursion with
no base case. Stopping leaves a state that is describable in one sentence — "actions 8..5 were
reversed; 4 failed because <error>; 3..1 were not attempted" — and the user can fix the cause and
re-run `flux undo`, which is idempotent-by-reconstruction because it re-reads the same stored
reverse actions.

### 6. The approval-time risk signal

The plan-approval sheet (C-182 lists the operations; C-154 tints by effect tier) gains one line
when any op in the batch declares `Compensation::None`:

> `⚠ 2 operations cannot be undone — send_email, bash`

This is the story's "no compensator declared becomes a policy-visible risk signal", and it is
available at approval time because *declaration* is static even though *materialization* is not.
Policy can additionally gate on it (`require_approval` for irreversible ops in protected scopes)
using the vocabulary that already exists.

## Non-goals

- **Not a database transaction.** There is no isolation and no atomic commit; effects are visible in
  the world the moment they happen. This is a *compensating* undo (a saga), which is the only shape
  available when the "resources" are other people's filesystems and APIs.
- **Not automatic.** flux never undoes a turn on its own. `flux undo` is a user verb.
- **Not a replacement for git.** For tracked files git is better and users should keep using it;
  this covers untracked files, out-of-repo paths, and non-filesystem effects git cannot see.
- **Not retroactive.** Turns recorded before this ships have no `Compensated` events and report as
  un-undoable, honestly.

## Verification

- Failing-first: `flux undo --turn N` on a turn that wrote a file restores its prior bytes —
  impossible today (no verb, no stored reverse).
- A registry-walk test failing on any mutating built-in op with no `Compensation` declaration.
- A test that a capture failure degrades to `None { why }` and is disclosed, never silently dropped.
- LIFO correctness: a turn writing the same path twice, undone, yields the pre-turn bytes.
- Partial failure: a turn whose middle compensator fails reports the exact boundary and leaves the
  remainder unattempted.
- Undo-through-the-envelope: a test asserting the undo batch hits the approval gate and the guarded
  system (it is not a privileged path).
- Full gate in both workspaces.

## Stories

| ID | Title |
|---|---|
| A-103 | The `Compensation` contract on `ToolSpec` + the registry-completeness test |
| A-104 | Pre-image capture at the dispatch seam; the `EventKind::Compensated` variant + projection |
| A-105 | `flux undo --turn <n>` — reverse-batch reconstruction, LIFO execution through the envelope, itemized report |
| A-106 | Irreversibility disclosure on the approval sheet + the policy hook |
