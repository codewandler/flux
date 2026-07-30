# Fleet loop — the fleet runs the track / impl-coord loop

**Status:** designed 2026-07-30. Epic tracker: [C-239](../stories/C-239-fleet-loop-epic.md).
Sibling epics: [fleet-coordinator.md](fleet-coordinator.md) (what 0.36.0 shipped),
[agent-fleet-runtime.md](agent-fleet-runtime.md) (the distributed half, deliberately later).

## The problem

flux 0.36.0 ships a fleet *coordinator*: a Program declares a work board
(`datasource board / kind "board:markdown"`) and hands items to remote flux agents over A2A via
`fleet.dispatch` / `fleet.status` / `fleet.cancel`, writing `runner` + `task_id` back so a restarted
coordinator can re-derive in-flight runs.

What it does not do is run the loop that Claude Code's `track` plugin runs:

> read a board → select a wave of independent items → give each its own isolated worker that
> implements, runs the project gate, and commits on a scratch branch → review the returned diff *as
> evidence* (re-run the failing-first test against the merge base) → up to two rework rounds back to
> the **same** worker → park after that → integrate **serially with a full gate after every merge**,
> reverting on red → write the bookkeeping.

Today that loop exists only as prose in a plugin. This epic makes flux run it.

## Decisions

### 1. Build the full loop

Not a research proof, not a bridge to the `track` plugin. The loop end-to-end.

### 2. The model reasons; the host enforces

This is the load-bearing call. A `WaveCoordinator` in the runtime mechanically performs the
irreversible, all-or-nothing, order-sensitive actions: isolation, gate, merge, revert, ledger. The
model performs wave selection and diff review.

The consequence is the point: the loop's invariants — **fenced ledger · gate after every merge ·
never implement · revert on red · park after two rounds** — hold *even when the model is wrong or
lazy*, because they are host behaviour rather than instructions. The `track` plugin can only
*describe* the ordering; flux enforces it. `fleet.integrate` is the sharpest instance: it is
impossible to integrate without gating, because the op does both or neither.

### 3. Coordination *prose* does not go into flux

The coordinator's reasoning — wave-selection heuristics, review standards — is content, not
mechanism. It belongs in a reference coordinator Program and its guidance, not compiled into flux.
flux ships the ops and the `WaveCoordinator`; the `.flux` Program is the example of using them.

## What already exists (verified against the tree, 2026-07-30)

An earlier audit claimed most of this was missing. That audit was wrong; this list is code-read.

- **Wave control flow** — `each`, `parallel` (+ `race`), `match`, `Try`/`catch`, `loop`/`repeat`,
  `route` (`crates/flux-lang/docs/syntax.md:512-760`).
- **A Program can run a gate** — `proc.run` plus `cargo_build/check/clippy/fmt/test`
  (`crates/flux-tools/src/cargo.rs`, all `AccessKind::Process`).
- **The `git_*` family**, less `branch`/`merge`/`revert`:
  `checkout/stage/stage_hunks/commit/diff/status/log/hunks/push/worktree_enter/worktree_leave`.
- **Structural write confinement** — path-scoped write authority with glob grants and lexical
  normalization (`crates/flux-policy/src/lib.rs:298-304`, `:589`, `:640`). A worker *can* be fenced
  to its write-set by construction rather than by instruction.
- **A2A session continuity on `contextId`** — `find_or_mint_session`
  (`crates/flux-server/src/a2a.rs:88`). This is the rework path: a second `fleet.dispatch` genuinely
  resumes the same worker.
- **`Task.artifacts`** as the spec-faithful home for a structured worker result
  (`crates/flux-a2a/src/types.rs:248`).
- **`A2aSpawner` + `LocalSpawner`** both implement `Spawner`; the `task` op's authority contract is
  the precedent (`crates/flux-orchestrate/src/lib.rs:1077-1090`).

## The gaps this epic closes

1. **The data path is prose.** Board ops carry no `output_schema`, and `render_compact` exposes only
   `title`/`state`/`attempts`/`assignee` — dropping `runner`, `task_id`, `depends_on`, `repo`,
   `evidence` (`crates/flux-capabilities/src/datasource/board.rs:605`). `each`/`parallel` has nothing
   typed to iterate. Compounding it, string-returning cognition ops return a JSON-**quoted** string
   (C-235), so even scraping the prose fails. **This is the lynchpin: a coordinator cannot reason
   over a board it can only read as prose.**
2. **No integration verbs.** `git_branch`/`git_merge`/`git_revert` are absent, so the serial, gated,
   revert-on-red half cannot be written at all.
3. **Worker isolation is session-local.** `git_worktree_enter` rebases the *caller's* root and
   forbids nesting (`crates/flux-tools/src/lib.rs:3147-3157`). It cannot give N parallel workers
   their own checkouts.
4. **The result path is half-designed.** `Task.artifacts` exists, but no worker emits a structured
   handoff and no coordinator op consumes one.
5. **The track contract lives nowhere.** Fenced ledger, disjointness, no-implement, the 2-round
   budget, gate-per-merge — all prose in a plugin, none enforced.
6. **Board correctness.** `transition` never clears `runner`/`task_id`, so a `Failed→Ready` retry
   keeps a stale runner and the next sweep chases a dead run
   (`crates/flux-capabilities/src/datasource/memory_board.rs:181-186`). `board.comment` is
   write-only. There is no reassign op. `depends_on` is stored and rendered but read by no
   computation. `Item::evidence` round-trips through the markdown format but nothing can write it.

## ⚠ Correction (2026-07-30) — isolation and result-return are not what the plan first assumed

Three findings from a deep code-read change Milestone 3 materially, and two contradict what
Milestone 2 originally assumed. They are recorded here because the epic's *scope boundary* depends
on them.

1. **Per-worker filesystem isolation does not exist for remote workers, and is designed out.**
   `git_worktree_enter` is caller-context-local by construction, and
   `fleet-coordinator.md:303-311` declares the isolation problem "dissolved" on the grounds that the
   worker owns its own workspace. So `fleet.isolate` — a worktree on the coordinator's machine —
   isolates a **local** worker only; a remote A2A worker can neither receive it nor be verified to
   have honoured it. **Consequence: for now the full code-implementation loop is a LOCAL-worker
   loop.** In-process children do get real worktree isolation via C-100
   (`SpawnRequest.system` → a fresh `WorkspaceContext`, `crates/flux-orchestrate/src/lib.rs:342-352`).
   A remote *code* worker needs `DockerRuntime` (A-124).
2. **A worker cannot return a branch or a diff — only text.** `SpawnOutcome` has no artifact field
   (`crates/flux-runtime/src/lib.rs:104-112`) and `flux-server` never populates `Task.artifacts`
   (the type exists; nothing sets it). A returned branch *name* is useless from a remote worker
   anyway — its branch lives on another filesystem with no fetch path. So "review the diff as
   evidence" has **no channel** for a remote worker. It works for a local worker because that worker
   shares the coordinator's `.git`.
3. **Nothing spawns a worker, and one worker serves one turn.** `flux` never spawns `flux`
   (`agent-fleet-runtime.md:13`), and `FlowEngine`'s `turn_gate` means one worker = one concurrent
   turn (`crates/flux-flow/src/engine.rs:172`). **`ProcessRuntime` is therefore not an optimization
   but a prerequisite for any wave larger than one.**

Two positive corrections from the same pass: A2A session continuity on `contextId` *is* implemented,
so rework to the same worker genuinely resumes it; and `SpawnActivitySink` *is* installed in
production — but only for local children (`crates/flux-flow/src/engine.rs:527`).

**What the correction changes:** Milestones 1–3a stand, because a local worker shares the
coordinator's `.git` and gets C-100 isolation. The remote/distributed story — Docker isolation,
artifact return over A2A, worker discovery, worker auth — is the `agent-fleet-runtime` epic and is
explicitly later. That epic is what turns "the loop runs on one machine" into "the loop spans
machines."

## The stories

| # | Story | What it lands |
|---|---|---|
| **F1** | [C-236](../stories/C-236-structured-board-query-comment-read-back-and-raw-string-cognition.md) | Structured `board.query` + `board.comments` read-back + raw-string cognition (fixes C-235) |
| **F2** | [C-240](../stories/C-240-board-correctness-retry-clears-runner-reassign-record-evidence.md) | Retry clears `runner`/`task_id`; `board.reassign`; `board.record_evidence`; the `Blocked→Ready` attempts hole |
| **F3** | [C-238](../stories/C-238-git-branch-merge-revert-ops.md) | `git_branch`, `git_merge`, `git_revert` |
| **F4** | [C-241](../stories/C-241-fleet-isolate-per-item-worktree.md) | `fleet.isolate` — a per-item worktree on the coordinator's machine |
| **F5** | [C-242](../stories/C-242-fleet-integrate-gated-merge-revert-on-red.md) | `fleet.integrate` — gated `--no-ff` merge, revert on red, by construction |
| **F6** | [C-243](../stories/C-243-fleet-start-process-runtime.md) | `fleet.start` + `ProcessRuntime` — the `AgentRuntime` port (absorbs A-120…A-122) |
| **F7** | [C-244](../stories/C-244-worker-template-and-fleet-handoff.md) | The implement-worker template + `fleet.handoff` (structured `Task.artifacts`) |
| **F8** | [C-245](../stories/C-245-fleet-rework-two-round-budget.md) | `fleet.rework` — same worker, 2-round budget as a host rule |
| **F9** | [A-117](../stories/A-117-coordinator-program.md) | The reference `coordinator.flux` + offline end-to-end journey |
| **F10** | [C-246](../stories/C-246-fleet-observability-spawn-activity-sink.md) | Observability — install `SpawnActivitySink`, per-worker status on the surface |

Ordered so the data path and the contract land *before* anything reasons over them: F1/F2 make the
board readable and correct; F3/F4/F5 make integration possible; F6/F7/F8 make the worker real;
F9 is the product; F10 makes a running fleet visible.

## Non-goals (deliberate)

- **`DockerRuntime` / `KubernetesRuntime` / NDJSON-stdio transport / endpoint-broker discovery**
  (A-123…A-126) — the loop is proven on `ProcessRuntime` first; those are later runtimes against the
  same `AgentRuntime` port.
- **`JiraBoard` / `GitlabBoard`** (A-115/A-118) — `MarkdownBoard` is the reference backend and the
  port is already proven swappable.
- **A flux-native replacement for the `track` plugin.** The reference `coordinator.flux`
  demonstrates the loop. Whether flux becomes the harness is a product decision, not this epic.
- **Remote code workers.** Per the correction above, out of scope until `agent-fleet-runtime` lands
  artifact return and container isolation.

## Verification

The epic's headline proof is F9's offline end-to-end journey: a stub A2A worker, a `MemoryBoard`,
two items — one integrating, one parking — with no network and no real model. Every story below it
carries its own failing-first test, named in its Acceptance.

The invariants that must be *mechanically* true when the epic closes, each pinned by a test rather
than by prose:

- Integrating without running the gate is impossible.
- A red gate leaves the integration branch at its pre-merge tree, via `revert -m 1` — never `reset`,
  never a rewrite.
- A third rework round cannot dispatch; it parks.
- A worker cannot write a fenced ledger path.
- A `Failed→Ready` retry leaves no stale `runner`/`task_id` for the next sweep to chase.
