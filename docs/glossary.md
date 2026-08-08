# Glossary — the delivery vocabulary

Every term the board and fleet contracts depend on, in one place, with **the distinction that makes
it non-obvious**. [concepts.md](concepts.md) is the product vocabulary — ops, runtime, datasources,
sessions. This file is the delivery vocabulary: what the board schedules, what the fleet executes,
and what each verb is allowed to claim when it reports success.

It is not a dictionary of the easy words. An entry exists here only because the contracts have had
to correct the same confusion more than once — *a worker handoff is not board completion*,
*`already-built` matches a mention, not an implementation*, *`applied` is not landed*. Prose
corrections scattered through `AGENTS.md` do not survive contact with a fresh agent; a checked list
of them might.

**Read this before acting on a board or fleet contract.** [AGENTS.md](../AGENTS.md) names it as
required reading for exactly that reason.

## How to read an entry

Each entry is a definition, then `Not:` — the confusion it exists to prevent — then `Anchor:`, the
CLI operation or source token the term actually names.
[`crates/flux-cli/tests/delivery_vocabulary.rs`](../crates/flux-cli/tests/delivery_vocabulary.rs)
checks every anchor still resolves, so renaming a verb or a field fails the build until this file
changes **in the same commit**. That is the whole mechanism keeping a glossary from becoming
archaeology.

---

## The two systems

### board

The answer to *what work exists and what is eligible next*. Scope (`session`, `repository`,
`workspace`), profile (`general`, `planning`, `execution`) and backend are independent axes, so
"the board" is always a resolved selection rather than a single thing.

- Not: a knowledge datasource, and not the fleet's database. The board never learns what a worker
  did; the fleet never owns a schedule of its own.
- Anchor: `board show`

### fleet

The answer to *who is executing an eligible item and what happened during delivery*. One local
supervisor per workspace, holding runtime state in `.flux/fleet/state.json`.

- Not: a scheduler. It consumes the resolved board; the board never reads fleet configuration.
  `.flux/fleet.toml` is execution policy, not planning state, and state plus events are runtime
  state, not roadmap truth.
- Anchor: `fleet status`

### workspace / member

A workspace board is several repositories bound together. A **member** is one of them: an id, a
root, a board and a canonical ref. Workspace item ids are spelled `member/ID`.

- Not: a copy. Each story stays authoritative in its owning repository; the workspace never
  duplicates status or Acceptance. A board member is also not automatically a fleet repository —
  planning membership (`[[members]]`) and execution membership (`[[repositories]]`) are separate
  tables, and a member absent from the second is never dispatchable.
- Anchor: `WorkspaceMember`

### canonical ref

The ref a member's items are *read at* and the ref accepted work *lands on*.

- Not: any ref you like. It must be writable by whatever lands on it — a remote-tracking ref such as
  `origin/main` moves only on fetch, and the fleet never pushes, so a member configured that way can
  never be delivered by any fleet operation. Being a read boundary has a second consequence agents
  trip on constantly: **an uncommitted story does not exist**, because items are read with
  `git ls-tree`/`git show` at this ref.
- Anchor: `canonical_ref`

---

## What is scheduled

### story

The smallest schedulable contract: one planning document whose `## Goal` and `## Acceptance` define
done. Created from [`docs/stories/_TEMPLATE.md`](stories/_TEMPLATE.md) — the one definition of a
story's shape, which `board create` generates from rather than restating.

- Not: an issue or a task. A story with no usable contract is not a small story, it is a story that
  cannot be dispatched, reviewed or completed — acceptance is machine-read by completion, by
  reconciliation, by the reviewer and by the driver.
- Anchor: `board create`

### acceptance / criterion

`## Acceptance` is the contract; each `- [ ]` line under it is one criterion. Ticking every box is
the precondition for completion.

- Not: a summary of the work. A criterion a machine cannot check is not a criterion, and *zero*
  criteria is indistinguishable from *all criteria satisfied* to any counter that only asks how many
  remain unticked.
- Anchor: `## Acceptance`

### goal

`## Goal` — the outcome the story delivers, in one or two sentences.

- Not: a title restated. A story whose Goal is the template's own placeholder has no goal; that is
  why creating one directly at `ready` is refused.
- Anchor: `## Goal`

### status

`backlog | ready | in-progress | blocked | done`, with a fixed transition table:
`backlog→ready`, `ready→{in-progress, blocked}`, `in-progress→{blocked, done}`,
`blocked→{ready, in-progress, done}`.

- Not: a free-text field, and not arbitrarily reachable — there is no `backlog→in-progress`, no
  `ready→done`, and nothing leaves `done`. Transition a story to `in-progress` **at dispatch, not at
  close**: implementors are fenced from story frontmatter, so a coordinator that waits shows active
  work as schedulable. Never hand-edit frontmatter; a hand edit bypasses validation.
- Anchor: `board transition`

### priority

An integer rank among `ready` items; lower is more urgent, ties broken by natural id.

- Not: importance in general. It exists **only while `ready`** — every transition away from `ready`
  nulls it — and it is deliberately subordinate to independence when a wave is being assembled.
- Anchor: `board next`

### area

The story's declared **write set**, as `areas: [...]`. It is what decides whether two stories can be
built at the same time.

- Not: a label or a component tag. A shared area is a conflict; declaring *no* areas is also a
  conflict, because an undeclared write set is an unknown one.
- Anchor: `areas`

### dependency

`depends_on` — a prerequisite item that must be `done` before this one is eligible.

- Not: a parallelism claim. A dependency is an ordering. Program dependencies may add
  cross-repository order but never remove a story's own.
- Anchor: `depends_on`

### epic

A slug grouping related stories under a shared outcome, rendered as a board grouping.

- Not: a dispatchable item, and not a status carrier. Children carry the statuses; the epic carries
  none.
- Anchor: `epic`

### design

A linked technical approach under `docs/designs/`, resolved from `design:`.

- Not: a queue item. It has no status and is never scheduled. Two stories sharing a design *do*
  conflict in a wave, because both edit that document as they land.
- Anchor: `board design`

### decision

A cross-repository architecture record: proposed, accepted, open or superseded.

- Not: an attention item by default. Only a genuinely *open* decision — one carrying a question,
  trade-offs and the items it blocks — blocks anything, and it blocks only its linked items.
  Superseding is partial by default: the successor names which points survive.
- Anchor: `board decision`

### evidence / comment

Append-only sections on an item: `## Evidence` records what proves the work, `## Comments` records
durable discussion.

- Not: frontmatter, and not handoff evidence. Handoff evidence is host-derived from the commit
  range; these two are authored.
- Anchor: `board evidence`

### done override

The recorded, non-empty reason required to complete a story with unticked criteria, persisted as
`done_override`.

- Not: a bypass flag. An empty reason is refused, and the reason travels into the result envelope —
  an escape hatch that leaves a trace, because the alternative is people writing `- [ ] it works`.
- Anchor: `override-reason`

---

## What decides eligibility

### check

`board check` validates the planning documents against each other: duplicate ids, a `ready` item
with no integer priority, a filename that does not start with its id, a `depends_on` or `design:`
naming something that does not exist, and every document still off the branch.

- Not: reconcile. Check reads documents; reconcile reads git.
- Anchor: `board check`

### reconcile

`board reconcile` reports items whose work appears already present while their status says
otherwise.

- Not: a repair. Detection is the whole value — it is read-only by construction, which is what makes
  it safe to run from a coordinator loop.
- Anchor: `board reconcile`

### already-built / implementation-landed

Reconcile's stronger signal: a commit reachable from `HEAD` that names the item **and** touches a
path outside the board's own documents.

- Not: proof of an implementation. It matches a *mention* of a story id. A design doc that names a
  symbol is a forward reference, not the symbol. It is a reason to withhold dispatch and ask a
  human, never a reason to close a story.
- Anchor: `already-built`

### acceptance-complete

Reconcile's weaker signal: every checkbox under `## Acceptance` is ticked.

- Not: a completion authority, and deliberately weak — it needs no history and no commit convention,
  so a repository with neither still gets an answer.
- Anchor: `acceptance-complete`

### next

`board next` returns dependency-satisfied `ready` items in priority order.

- Not: a statement about the tree — it answers a question about *status*. And dependency
  satisfaction says only that nothing an item waits on is outstanding; it says nothing about whether
  two items can be built at the same time.
- Anchor: `board next`

### independent set (the "batch")

`board next --independent` returns the **largest** mutually independent set rather than the
highest-priority prefix, because more items always beats better priority: an idle worker delivers
nothing while a lower-priority story still delivers a story. Independence fails closed.

- Not: a wave. This is a *proposal* — a set of items that may safely be built together. It becomes a
  wave only when something dispatches it. The held-back list it returns alongside is as much the
  point as the set is.
- Anchor: `independent`

### revision

A content hash of the whole board, returned by every mutation and accepted by `--if-revision`.

- Not: a lock. `--if-revision` is a precondition: a mismatch is refused as a stale revision. A
  mutation that returns **no** revision wrote nothing — which is exactly what `--dry-run` does.
- Anchor: `if-revision`

### the board's own documents

The document roots `board commit --all` may reach: stories, designs, decisions, vision, roadmap.

- Not: everything dirty in the checkout. The fence is structural rather than a rule someone has to
  remember — a source edit, a manifest or a lockfile is out of reach by construction, because
  another session's work is not this board's to commit. `CHANGELOG.md` is authored but excluded even
  so: the most contended ledger in a repository has to be a deliberate act.
- Anchor: `board commit`

### milestone

The active cross-repository outcome boundary. Exactly one is active for scheduling; lanes name the
milestone they belong to.

- Not: a bound on the ready pool. **The driver dispatches from the active milestone's program, while
  `board next` reads the whole ready pool** — the single most common explanation for "the fleet is
  idle but there is ready work". Completion is evidence-bound, and never automatic.
- Anchor: `active_milestone`

### lane

One ordered program slot binding one workspace item to a milestone, with its own order, outcome and
cross-repository dependencies.

- Not: a status carrier, and not a work-splitting device. A lane cannot hold a second copy of a
  story's status, and stories are never split into API/UI/test/docs lanes — independent contracts
  come first. Where lanes exist, lane `order` is the schedule and story `priority` is not consulted.
- Anchor: `ProgramLane`

### program

The native catalogue of ordered lanes in `.flux/board.toml` — the schedule itself.

- Not: prose. README and AGENTS files explain policy; nothing parses them as schedule data. And when
  a program exists it is exclusive: unrelated ready repository stories are not fallback work.
- Anchor: `active_program_projection`

### configured wave

A board-owned dispatch template: one repository, at most ten exact item references, stable order,
declared wave dependencies.

- Not: a dispatched wave. This is immutable planning input; the dispatched instance is mutable
  execution state. Ten is a capacity limit, not a quota. See **wave** below — the unqualified word
  is the most-mangled term in the vocabulary.
- Anchor: `ProgramWave`

---

## What executes

### wave

A **dispatched wave instance**: one durable fleet record holding the repositories, stories,
branches, base commits and worktrees of one parallel execution, advancing
`awaiting-handoffs → handoffs-ready → integrating → green|red|conflict → applied|awaiting-delivery`,
with `parked`, `cancelled` and `abandoned` off to the side.

- Not: the set of stories, and not the configured wave that named them. A wave is the record that
  *claims* items and owns the disk they are built on.
- Anchor: `fleet run`

### claim

A wave's exclusive hold on the board items it was dispatched for, which removes them from the
dispatch pool for as long as the wave holds work.

- Not: a lock, and not a conflict. A held reservation is a queue. The rule that costs the most when
  forgotten: a *parked* wave holding a commit keeps its claim — that work is
  delivered-pending-integration, not re-dispatchable — while an *empty* parked wave must release
  its claim or the frontier deadlocks.
- Anchor: `wave_still_claims_items`

### worker

One admitted agent with exactly one story, one branch and one isolated worktree.

- Not: every agent in the process tree. Nested task children are not additional fleet workers, and
  the unique coordinator is never a worker. Workers start fresh with their exact assignment, loop
  binding, ceiling, budget and fence; they do not inherit the coordinator's conversation and do not
  select follow-up work.
- Anchor: `main_agent`

### turn

One synchronous model execution inside the process that recorded the worker as working.

- Not: evidence about the tree. **A turn recorded `success` may hold no commit, and a turn recorded
  `failed` may hold a complete one — check the worktree, not the status.** A turn that stopped with
  no `turn_end` was *interrupted*, not failed, and its worktree may hold uncommitted work.
- Anchor: `turn_end`

### loop

An operator-authored Flux-Lang program bound to an agent at admission, by profile id, revision,
entry point, immutable source reference and digest.

- Not: inferred, defaulted or hot-reloaded. The fleet never guesses a profile from issue prose and
  never falls back to a generic loop; editing the file affects only the *next* admission. Note the
  collision: `fleet drive --loop` is an interval repeat, a different sense of the word entirely.
- Anchor: `loop_profiles`

### admission / admitted operation

Admission is the freeze point before the first model call: the fleet resolves the profile, validates
required operations and runtime features, and snapshots the binding. An **admitted operation** is
one name inside the resulting closed catalogue.

- Not: configuration, and not presence. Neither a config entry nor a process on the network makes
  something a fleet member. Drift from the snapshot requires explicit re-admission, never a silent
  fallback — and the real ceiling is the installed operation set, not the declared capability list.
- Anchor: `task_kind_native_operations`

### write fence

A path glob an admitted agent may not write, snapshotted into its capability set. `.git/**` and
`.flux/fleet/**` are always prepended, whatever a template declares.

- Not: advice, and not a secrets mechanism only — shared ledgers are fenced rather than merely
  discouraged. A fence is also only as good as the operation set around it: one general shell call
  defeats fences, typed effects and the operation allow-list together. Narrowing a fence needs no
  permission; widening one always does.
- Anchor: `normalize_fences`

### width

How many workers may run at once.

- Not: a throughput target. It is a ceiling, and in practice a *disk* decision — a story held back by
  width is a fact about the fleet, not a judgement about the item.
- Anchor: `max_workers`

### drive / tick

`fleet drive --tick` runs one deterministic pass — report, advance, accumulate, dispatch — and
`--loop` repeats it under a single-instance guard.

- Not: the whole pipeline. **Drive never calls integrate, apply, resume or reap**, and nothing in a
  tick writes a member's canonical ref, so waves stop at `handoffs-ready` under drive alone. Only
  `--loop` takes the single-instance lock; `--tick` does not.
- Anchor: `fleet drive`

### withhold

A tick declining to dispatch an otherwise-ready item, with a recorded reason and run length.

- Not: a schedule. A withhold that repeats is a decision no one reviewed. The dispatch check fails
  closed: if reconcile cannot be read, the tick does not dispatch at all.
- Anchor: `withheld`

### quiesce

A durable, recorded stop of dispatch that fails while any worker turn is still in flight.

- Not: the absence of a process, and not `fleet stop`. Inspection, handoff, integration, acceptance
  and reclamation stay available while quiesced — a maintenance window that also blindfolded the
  operator would just be a worse stop.
- Anchor: `fleet quiesce`

---

## What is produced

### handoff

The host-verified record that one worker produced one exact commit, with a derived write set and
typed validation evidence. It sets that story to `handoff-accepted` inside the wave.

- Not: board completion. **A worker handoff is not board completion** — host verification, review,
  integration and the repository gate all come first, and push, release, deployment and milestone
  promotion are separate explicit actions after that. A green handoff is also not a green story: it
  verifies the argv it was given, which is what the repository gate is for. `handoff-accepted` is a
  wave status and never appears in frontmatter.
- Anchor: `handoff-accepted`

### review / rework

A fresh read-only assessment of the exact handoff commit against the story contract, returning a
typed pass, rework or park.

- Not: the writer's own judgement, and not optional. An agent that can edit the code it judges is
  not a reviewer; a review that could not run records `examined: false` rather than a pass; a
  malformed verdict never becomes a pass, and a `PASS` carrying findings is malformed rather than
  generously reinterpreted.
- Anchor: `fleet review`

### park / unpark

A recorded pause on a wave, with a mandatory reason and the status it will return to.

- Not: blocked, and not an ending. `blocked` is a *board item* status; parking is a *wave* state, and
  a parked wave keeps its claim because the work it holds is still on disk. Parking exists so a
  human's deliberation stops being re-decided every minute. Every branch that can end a wave hands
  off before it judges it.
- Anchor: `fleet park`

### capture

`fleet capture` commits the work a worker left uncommitted onto that story's own branch.

- Not: a delivery, and not a failure report. An interrupted turn leaving uncommitted work is the
  normal state; capture is how that work stops being the only copy on disk. It reports a verified
  effect, because `git commit` exiting 0 is not proof a commit was made.
- Anchor: `fleet capture`

### reclaim / reap

Reclaim deletes a terminal wave's regenerable build output and removes worktrees **only** when they
provably hold nothing. Reaping cancels a wave and removes its worktrees.

- Not: symmetric, and never destructive of work. Build output goes unconditionally; a worktree is
  removed only when it has no uncommitted change and no commit unreachable from the canonical ref. A
  single uncommitted byte aborts a reap — and the way past that is to *commit* the work, never to
  relax the check. A story worktree is the only place an interrupted worker's work exists; an
  integration or verify checkout is an assembly that can be rebuilt.
- Anchor: `fleet reclaim`

### abandoned

A wave whose claim is released but whose disk is not disposable.

- Not: cancelled. Cancelling is an ending, and an ending makes the wave's disk disposable — which is
  exactly the mistake that costs an unrecoverable worktree.
- Anchor: `abandoned`

### integrate / candidate

`fleet integrate` assembles a wave's accepted story commits, in dependency order, into one
**candidate** per repository, then runs that repository's gate over it exactly once.

- Not: one candidate per wave — a wave can hold a green candidate beside a conflicted one. Not a
  per-story gate either: a green targeted test is story evidence, a green final gate is wave
  evidence. A dependent story starts from the integrated prerequisite, not from the original commit.
  Neither `green` nor `applied` is reflected on the board.
- Anchor: `fleet integrate`

### gate / prepare

The repository's own final command set, run once per dispatched wave instance in the integration
worktree; `prepare` is the regeneration that runs first.

- Not: per story, and not the story's own targeted checks. Regenerated artifacts belong to the
  candidate, not to any one story. An armed golden regeneration fails on purpose, so its status
  carries no information until the run is repeated unarmed.
- Anchor: `prepare`

### apply

`fleet apply` accepts a recorded green candidate and pins it with an annotated tag.

- Not: a merge, and not delivery. It reports `merged_locally: false`, and it stops one step short on
  purpose. `applied` may only be recorded once the canonical ref is *observed* to contain what the
  wave accepted; otherwise the wave records `awaiting-delivery` — accepted, with only landing
  outstanding.
- Anchor: `fleet apply`

### promote / land

`fleet promote` is the only operation that writes a member's local canonical branch: per member, in
declared order, it accumulates the accepted candidates the ref lacks, merges and gates them in a
throwaway worktree, then advances the branch by a compare-and-swap ref update.

- Not: a merge in a checkout, and not a push. A candidate that will not combine is excluded and
  named, never forced; a red gate anywhere leaves every member's branch untouched; and the verdict
  is re-read from git, so `landed` describes the ref rather than the attempt.
- Anchor: `fleet promote`

### publication / release

Landing is commits reaching a member's canonical ref. **Publication** is that ref reaching its
declared upstream. **Release** is a version being cut and artifacts made public.

- Not: three names for shipping. They are three separate events with separate preconditions, and the
  fleet's autonomous job is only the first. Local acceptance is not publication; nothing crosses an
  outward boundary without a verified precondition.
- Anchor: `fleet promote`

### doctor

`fleet doctor` reports structural findings — a claim held by a wave with no live supervisor, a
canonical ref nothing can write, an unrecorded worktree.

- Not: a repair, and not a gate. Runtime findings do not fail the run; `data.runtime` is the
  machine-readable verdict.
- Anchor: `fleet doctor`

### repair

`fleet repair` rebuilds recorded structure for a wave whose state lost track of it.

- Not: a rewind. It discards nothing and never resets a story worktree.
- Anchor: `fleet repair`

### harvest

Turning finished-but-unrecorded worker output into recorded handoffs before some other action buries
it — done implicitly by parking and by the driver.

- Not: a verb. There is no `fleet harvest`; recovery is the half of the fleet that has no verbs, and
  the operator-scoped pass that turns a parked backlog into delivered candidates is assembled from
  `capture`, `repair`, `integrate` and `apply` by hand.
- Anchor: `record_turn_handoffs`

### envelope

Every command returns `{schema, ok, request_id, revision, data, warnings, error}`; errors carry a
class that maps to an exit code (`input/schema` 2, `not-found` 3, `conflict/precondition` 4,
`permission` 5, `transient-worker` 6, `validation/gate` 7).

- Not: two surfaces. Human output is a projection of the same JSON values; JSON/NDJSON is the
  automation API.
- Anchor: `flux.cli/v1`

---

## Words that are not our words

A story that renames a concept is how the vocabulary drifts, and a rename in prose becomes a rename
in code two stories later. `crates/flux-cli/tests/delivery_vocabulary.rs` refuses these exact
phrasings in `docs/stories/`.

The phrases are deliberately narrow — the collocation, not the bare word — because the bare words
have honest uses here: `batch` names the independent set, `block` is a real board verb, `lock` names
an OS lease around the build directory. A lint people suppress is worse than no lint, so this one
flags only phrasings that can mean nothing but the renamed concept, and matches whole words only:
`block the story` is not a finding for `lock the story`.

The lint reads every story, so this table is the only place those phrasings may be written down. A
story that needs to discuss the vocabulary points here rather than quoting the words — which is
itself the discipline the glossary is for, and the first thing this lint caught was the story that
introduced it.

One more word is retired without being linted. **`tranche`** was removed from active scheduling
contracts — milestones and lanes replaced it — but historical prose and changelog records keep the
ordinary English word, so flagging it would produce a dozen findings nobody may act on.

| Do not write | Write instead | Why |
|---|---|---|
| `batch branch`, `batch worktree`, `batch of stories`, `dispatch a batch` | wave | A batch is what `board next --independent` proposes; a wave is the record that claims and executes it. |
| `lock the item`, `lock the story`, `item lock`, `story lock` | claim | A claim is a queue, not mutual exclusion, and it survives a parked wave. |
| `block the wave`, `blocked wave`, `unblock the wave` | park / unpark | `blocked` is a board item status; a wave parks, keeps its claim, and returns to a recorded status. |
| `pause the wave` | park | A pause leaves no reason and no return status; a park records both. |
| `merge the candidate` | apply, then promote | `apply` merges nothing — it pins a tag. Only `promote` moves a branch. |
| `worker seat` | worker | A worker is one admitted agent with one story; the fleet has no seat abstraction. |

## Related

- [concepts.md](concepts.md) — the product vocabulary: ops, runtime, datasources, sessions.
- [../AGENTS.md](../AGENTS.md) — the repository contract this vocabulary serves.
- [stories/README.md](stories/README.md) — the board this vocabulary describes.
