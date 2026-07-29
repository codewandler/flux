# Fleet coordinator — flux orchestrating flux across repos

Story: [A-111](../stories/A-111-fleet-coordinator-epic.md) · Pillar: Agent · Status: design

## What we set out to build, and what the tree said

The ask was a **first-level orchestration harness**: something that handles cross-repo work, talks
to Jira, holds a global board, dispatches work to remote agents, monitors them, and reports back.
The assumption going in was that this is a *new app* — a second harness beside the coding agent,
with its own supervisor loop.

Reading the tree says otherwise. **`flux-app` is already that harness.** It runs a `.flux`
`Program` declaring `agent` / `channel` / `datasource` / `trigger` / `journey`, owns an event bus, a
delivery supervisor (`crates/flux-app/src/supervisor.rs:42`) and an orchestration op-pack, and
`flux-channels` supplies the external I/O adapters — `schedule` (cron), `webhook`, `slack`, and an
`a2a` channel that *serves* a declared agent. `plugins/jira` already has issue CRUD, transitions,
comments, search, and `jira.issues` / `jira.users` datasources; `plugins/gitlab`, `slack`,
`confluence` and `opsgenie` sit beside it.

So the coordinator is a **Program**, not a binary. What is genuinely missing is five things, and
only two of them are large:

1. a **write-capable state port** — the board (§2, §3);
2. **outbound A2A dispatch** — the client half is unreachable from any journey or op (§4);
3. **run state** — which turns out to dissolve into the board rather than needing a store (§5);
4. **per-delivery bus isolation** — the load-bearing blocker (§6);
5. **multi-root workspaces** — which mostly dissolves once workers are remote (§7).

## 1. The parent shape already exists, and it is read-only

`flux-capabilities::LiveDatasource` (`crates/flux-capabilities/src/datasource/live.rs:60`) is
already the port shape this design wants for a state source:

```rust
pub trait LiveDatasource: Send + Sync {
    /// Model-facing entities, filter contracts, and page bounds.
    fn schema(&self) -> LiveSchema;
    /// Concrete external resources needed by `list` and `get`. Empty means an in-process backend.
    fn access(&self) -> Vec<LiveAccess> { Vec::new() }
    async fn list(&self, ctx: &ToolContext, entity: &str, page: PageRequest, filters: &Filters)
        -> Result<Page<Row>>;
    async fn get(&self, ctx: &ToolContext, entity: &str, id: &str) -> Result<Option<Row>>;
}
```

A backend declares its entities, typed filters and page bounds, plus its external authority
(`LiveAccess::Network { subject }` / `Connection { subject }`, `live.rs:24`). It is validated once
at registration, and the host then **generates uniform ops** — `<domain>.list` / `<domain>.get` —
with stable `<domain>/<entity>` permission subjects, a per-domain `ToolGroup` and an ambient signal,
registered atomically on a clone (`try_register_live_datasource`, `live.rs:130`).

That is the convention the board should follow. **But `LiveDatasource` is strictly read-only** —
`list` + `get`, nothing else. A board needs create, transition, claim and comment. That
write-capable sibling is the centre of this epic.

## 2. The `WorkBoard` port — the abstraction exposed to the agent

The state source is deliberately **abstract**: Jira is one implementation, a markdown file store
(the `docs/stories` + `/track:board` pattern flux already dogfoods) is another, and the coordinator
agent only ever sees the generated ops.

- **L0 contracts** — a new `board` module in `flux-datasource` (L0, `flux-codegate/src/lib.rs:34`),
  reusing `live::{Page, PageRequest, Filters, FilterValue, Reference}` verbatim rather than minting a
  parallel vocabulary:

  ```rust
  pub struct Item {
      id, title, state, assignee, runner, task_id,
      depends_on: Vec<String>, repo: Option<String>, attempts: u32, evidence: Vec<Reference>,
  }
  pub struct ItemDraft { … }
  pub enum State { Ready, Claimed, InProgress, Review, Done, Blocked, Failed }
  pub struct BoardSchema { … }   // mirrors LiveSchema: filters, page bounds, capabilities
  ```

- **L5 port + registration** — a `board` module in `flux-capabilities` (L5):

  ```rust
  #[async_trait]
  pub trait WorkBoard: Send + Sync {
      fn schema(&self) -> BoardSchema;
      fn access(&self) -> Vec<LiveAccess> { Vec::new() }
      async fn list(&self, ctx: &ToolContext, filters: &Filters, page: PageRequest) -> Result<Page<Item>>;
      async fn get(&self, ctx: &ToolContext, id: &str) -> Result<Option<Item>>;
      async fn create(&self, ctx: &ToolContext, draft: ItemDraft) -> Result<Item>;
      async fn transition(&self, ctx: &ToolContext, id: &str, to: State) -> Result<Item>;
      async fn claim(&self, ctx: &ToolContext, id: &str, assignee: &str) -> Result<Item>;
      async fn comment(&self, ctx: &ToolContext, id: &str, text: &str) -> Result<()>;
      // A-130, and the subject of §5: the write that makes the board a run registry.
      async fn record_dispatch(&self, ctx: &ToolContext, id: &str, runner: &str, task_id: &str) -> Result<Item>;
  }
  ```

  `try_register_work_board(registry, domain, backend)` generates `board.list` / `.get` / `.create` /
  `.transition` / `.claim` / `.comment` / `.record_dispatch`, following `live_datasource_tools` /
  `try_register_live_datasource` exactly: snapshot the contract, validate once, register atomically
  on a clone, return a surface carrying the group and the ambient signal.

### Why a purpose-built port and not "LiveDatasource plus mutations"

Two alternatives were weighed and rejected:

- **Extend `LiveDatasource` with optional mutating methods.** One port, one registration path — but
  every read-only backend then carries no-op writes, and the trait's meaning ("a read projection of
  an external system") stops being true. Worse, it cannot carry the state machine below.
- **A generic mutable-record port** — CRUD over typed rows, the work-item shape declared *on* it as
  schema. Maximally reusable, and exactly wrong here: the coordinator's whole value is that it
  **reasons** about the board — computes dependency waves from `depends_on`, detects stuck items
  from `state` + `attempts`, rebalances `assignee`. Opaque rows push all of that into prompt text.

The typed state machine is the reason the port is purpose-built:

```
spine:    Ready → Claimed → InProgress → Review → Done   (Done is terminal)
blocked:  {Ready, Claimed, InProgress, Review} → Blocked → Ready
failed:   {InProgress, Review} → Failed → Ready          (retry, attempts += 1)
```

`Failed` is reachable from `InProgress`, not only from `Review`: a crashed worker is *in* `InProgress`,
and that is precisely the state §5's sweep inspects — a machine that could only fail out of `Review`
would leave crashed items with no legal edge home. `Blocked` rejoins at `Ready` rather than `Claimed`,
so an unblocked item is re-claimed through the normal path instead of inheriting a stale assignee.

`transition` **validates the edge**; an illegal edge is an error, *not a write*. That is a property
a generic record store cannot express, and it is what makes a crashed coordinator recoverable (§5).
The edge set lives in one `const EDGES` table in L0 (`flux_datasource::board`); `State::allowed_next`
and `validate_transition` are its only readers, so this diagram and the code cannot drift apart
silently — the contract suite pins every edge above.

### The safety surface: concrete permission subjects

`LiveDatasource` never had to answer this, and the board must. The five mutating ops need accurate
`effects`, `Risk`, `Idempotency` and — critically — **concrete `permission_subjects`**
(`<domain>/item/<id>`), never `*` and never empty. AGENTS.md:98 is explicit:

> **`permission_subjects` must be accurate.** A tool declaring a `Write` effect but reporting no
> subjects is forced to approval — an unscoped write would otherwise match a `*` path grant. Don't
> return empty subjects to dodge gating.

So `board.transition` on `PROJ-42` reports `board/item/PROJ-42`, and a grant scoped to one project
cannot silently move another. `board.create` has no id before the call: it reports
`<domain>/item/new`, which is a *deliberately* distinct subject a policy can grant separately from
mutation of existing items. This is the story's main review surface, not an afterthought.

## 3. Backends — one shared contract-test suite, four implementations

| Backend | Notes |
|---|---|
| `MemoryBoard` | Offline test double, mirroring `MemoryBackend` (`flux-capabilities/src/datasource/memory.rs`). It is what makes the port's contract suite runnable without credentials — AGENTS.md demands offline-first tests. |
| `MarkdownBoard` | File-per-item + frontmatter, generated index — the `docs/stories` + `/track:board` pattern. **All IO through `flux_system::Workspace`**, never `std::fs`. |
| `JiraBoard` | Maps onto the existing `plugins/jira` ops (`jira.issue.create`, `jira.issue.transition.run`, `jira.issue.comment.add`, `jira.issue.search`, `jira.issue.edit`). The Jira-status ↔ `State` mapping is **config, not code** — Jira workflows differ per project, and hardcoding a transition name makes the backend work at exactly one company. |
| `GitlabBoard` | Proves the port is not "Jira with a trait on top". Deferrable past the epic's first release. |

One shared contract suite runs against all four: legal edges succeed, illegal edges error without
writing, `claim` is idempotent for the same assignee and conflicts for a different one, and `list`
honours the declared filters and page bounds.

**`MarkdownBoard`'s design risk is write contention** with N concurrent agents. Resolve it
structurally rather than with a lock: one file per item (no shared mutable file on the write path)
plus atomic write-then-rename; the index is **derived and never authoritative**, regenerated on
read. Two agents claiming different items never touch the same bytes; two agents claiming the *same*
item resolve by compare-and-set on the item file.

## 4. Outbound A2A dispatch — the client half is the gap

Workers are remote flux agents reached over **A2A**, not in-process sub-agents. That choice is what
makes the fleet survive a coordinator restart and lets each worker own its own repo checkout.

The **server** side is already complete, and it is worth being precise about where: the stateful
task model (A-53…A-57, done 2026-07-08) lives in `crates/flux-server/src/a2a.rs` — non-blocking
`message/send`, `tasks/get` (`a2a.rs:1262`), `tasks/cancel` firing the live entry's
`CancellationToken` (`a2a.rs:1315`), `tasks/resubscribe`, and push-notification config. Note that
`flux_a2a::server::is_unsupported_a2a_method` (`crates/flux-a2a/src/server.rs:195`) still classifies
those methods as unsupported — that is the *embeddable* reduced dispatch, not the served surface.
**Fleet workers must therefore be served by `flux serve` / flux-server**, and the design says so
rather than leaving it to be discovered at integration time.

The **client** side is the gap, in two ways:

- `A2aClient` (`crates/flux-a2a/src/client.rs:43`) exposes `send(message, blocking)`, `get_task`,
  `await_task` and `stream` — **but no cancel**, even though the server implements it.
- Nothing but the `flux a2a` REPL can reach it: the only callers outside the crate are
  `crates/flux-cli/src/a2a_cmd.rs:131` and `:218`. No journey and no op can dispatch to a remote
  agent.

The layering works out with no new crate and no `flux-codegate` `layer()` change: `flux-a2a` is
**L1** (`flux-codegate/src/lib.rs:41`), `Spawner` / `SpawnRequest` / `SpawnOutcome` are **L2**
(`crates/flux-runtime/src/lib.rs:573`, `:624`), and `flux-orchestrate` is **L3** (`lib.rs:46`) — so
an A2A-backed spawner in `flux-orchestrate` is a legal downward edge.

Two halves, because they answer different questions:

- **`A2aSpawner: Spawner`** — the blocking delegate case. `spawn(SpawnRequest, cancel)` maps onto
  `A2aClient::send(msg, blocking = true)`, and the passed `CancellationToken` becomes the new
  client-side `cancel_task`. This reuses the existing `task` op **verbatim**: zero new op surface,
  and every existing depth/cap-scope bound (A-25) still applies.
- **`fleet.dispatch` / `fleet.status` / `fleet.cancel`** — fire-and-**track**, which `Spawner`'s
  fire-and-await signature cannot express. `dispatch` wraps `send(blocking = false)` and returns the
  `task_id`; `status` wraps `get_task`; `cancel` wraps the new `cancel_task`. These are the ops the
  sweep journey uses.

## 5. Run state — dissolved, not built

There is **no second store**. `fleet.dispatch` writes the returned `task_id` and the worker's
`runner` URL back into the board `Item` — via `<domain>.record_dispatch`, the op the subsection below
specifies — so **the board is the run registry**. Monitoring is the
`sweep` journey on a `schedule` channel: for each `Claimed`/`InProgress` item, call `fleet.status`
and `board.transition` accordingly — cron-driven reconciliation, not an in-memory supervisor table.

Crash recovery is then free: restart, sweep, re-derive. Nothing was held in RAM that mattered. This
is the concrete payoff for §2's typed state machine — reconciliation is only sound if the set of
legal states is closed and every write went through an edge check.

### The op that performs the write-back (A-130)

The paragraph above was a claim until A-130: `Item` carried `runner` and `task_id` as *fields* that
no operation could set. The board gains a **seventh operation** rather than an eighth field:

```rust
async fn record_dispatch(&self, ctx: &ToolContext, id: &str, runner: &str, task_id: &str) -> Result<Item>;
```

generated as `<domain>.record_dispatch` with the same `<domain>/item/<id>` subject as every other
mutating op, `Effect::Write`, `Risk::Medium` and `Idempotency::Conditional` — replaying the same
`(runner, task_id)` rewrites the same two fields with the same values, which is exactly the stated
condition `Conditional` exists for and is *not* `Idempotent` (that would license the op cache to
skip the call and silently drop the write).

**Why a distinct op and not `claim(id, assignee, runner, task_id)`.** Atomicity with the claim is
the obvious argument for folding it in, and it does not survive contact with the ordering: the
`task_id` does not exist until the worker answers the send, so the record is necessarily written
*after* `claim` either way. Extending `claim` would therefore buy atomicity of `(assignee, runner,
task_id)` with the state change while leaving the only window that actually matters — between the
worker accepting the run and the board recording its id — exactly as wide. It would also make
`claim`'s `Conditional` idempotency incoherent: "same assignee, different `task_id`" has no answer.
Keeping `transition` as the single edge-checked entry into the state machine is the same argument
from the other side, which is why `record_dispatch` writes those two fields and moves nothing else —
no edge, no `attempts`, no assignee.

**Every backend owes it.** `record_dispatch` is a *required* `WorkBoard` method, not a defaulted one:
a board that silently declines to record is indistinguishable from a working one until a coordinator
restarts and recovers nothing, and the whole point of §5 is that this cannot happen. The shared
contract suite pins the property (durable across a fresh read, replacing on a redispatch, moves no
state), so a new backend either answers the question or fails the suite. `MemoryBoard` is the
reference implementation; [A-114](../stories/A-114-markdown-board.md)'s `MarkdownBoard` and
[A-118](../stories/A-118-gitlab-board.md)'s `GitlabBoard` each owe one.

**The window that is left, and what happens in it.** `fleet.dispatch` takes an optional `item` and,
wired to a ledger, records the dispatch before reporting success. Wiring is the assembler's job —
whoever registers the fleet ops constructs `FleetDispatchTool::with_ledger(BoardLedger::new(domain,
board))`; an op left un-wired still dispatches but refuses any call that names an `item`. A task
whose id was lost is
strictly worse than one never dispatched — nothing will ever sweep it and it holds a worker
indefinitely — so the two failure paths are decided rather than left implicit:

- **`item` named, no ledger wired** — refused *before* any network call. Dispatching first and
  discovering afterwards that the run cannot be recorded is precisely how an orphan is made.
- **Accepted, then the board write failed** — the op fires a compensating `tasks/cancel` on the run
  it cannot track, so nothing is left executing. If even that fails it reports `ORPHANED RUN` with
  the task id and a manual `fleet.cancel` recovery line, because at that point a human is the only
  remaining sweep.
- **A worker that answers synchronously** returns no task, so there is nothing to record and nothing
  to sweep; the call reports `"recorded": false` rather than storing a dead id that would send the
  next sweep after a run that no longer exists.

**Layering.** `fleet.dispatch` is L3 (`flux-orchestrate`) and `WorkBoard` is L5
(`flux-capabilities`), so the caller can never name the board. The seam is
`flux_runtime::DispatchLedger` (L2, beside `Spawner`, which is there for the same reason); the
adapter is `flux_capabilities::BoardLedger`. Both sides derive the item's permission subject from
one helper, so the fleet op and `<domain>.record_dispatch` cannot name the same item two ways.

**Deliberately unspecified:** recording a dispatch against a `Done` item is *not* refused. Refusing
on terminal states is scheduling policy this epic has not settled — whether a late record is a bug
or a benign echo of a race with the sweep depends on the sweep's own semantics — so the port stays
minimal (the id must exist; the write is a replace) and the question is left open rather than
answered by accident in one backend.

## 6. Per-delivery bus isolation — the load-bearing blocker

`flux-channels` states the constraint in its own module docs
(`crates/flux-channels/src/lib.rs:20`):

> Deliveries are **serialized by the shared `flux_app::App`**: `App::deliver` subscribes to the
> broadcast bus and drains the cascade events its journeys emit, so concurrent deliveries would
> double-process via broadcast fan-out. Owning the coordinator there covers direct calls and every
> adapter instance. **Cross-channel parallelism needs per-delivery bus isolation.**

`App::deliver` (`crates/flux-app/src/app.rs:501`) runs every journey a label triggers *to
completion*, including the cascade its journeys `emit`.

**Consequence for this epic:** a coordinator whose nightly sweep blocks webhook intake is
single-threaded by construction. A sweep over fifty in-flight items would stall every inbound Jira
webhook behind it. Nothing else in this design matters until this lands, which is why it is the
first story.

The seam already exists — `DeliveryOrigin`, a task-local (`crates/flux-app/src/bus.rs:23`, `:27`),
and `scope_delivery` (`bus.rs:231`). So the work is **scoping cascade collection to the causing
delivery and making `deliver` re-entrant**, not inventing a mechanism. It is likely breaking for
`flux-app` embedders ⇒ pre-1.0, that is a **MINOR**.

## 7. Multi-root workspaces — mostly dissolved by choosing A2A

`WorkspaceContext` (`crates/flux-runtime/src/lib.rs:815`) holds one `active: Arc<System>` plus an
optional `WorktreeSession`. That looks like a cross-repo blocker — a coordinator touching five repos
against a single guarded root.

Choosing remote A2A workers dissolves it: **each worker owns its own workspace pinning**, and
`SpawnRequest.system` (`lib.rs:581`) already carries a per-child system snapshot from C-100 for the
local case. What remains is narrow: `MarkdownBoard` may live in a different root than the
coordinator's cwd, which is a `System` *construction* detail, not a context-model change. **No
`WorkspaceContext` change is proposed** — this is folded into the `MarkdownBoard` story rather than
filed separately.

## 8. The coordinator Program

```
coordinator.flux                          # a Program, run by flux-app
  datasource board  { kind = "markdown" | "jira" | "gitlab" }
  channel jira_hook { kind = "webhook"  }   # issue created/updated
  channel nightly   { kind = "schedule" }   # the reconciliation sweep
  channel inbox     { kind = "a2a"      }   # humans/agents talk TO the coordinator
  channel ops       { kind = "slack"    }   # status updates out
  agent   coordinator { tools = [board.*, fleet.*, jira.*] }
  trigger on "jira_hook" run intake
  trigger on "nightly"   run sweep
  journey intake    -> board.create / board.transition
  journey dispatch  -> fleet.dispatch(A2A) -> board.claim(task_id)
  journey sweep     -> each claimed item: fleet.status -> board.transition / comment
```

`flux run coordinator.flux --serve` then supervises a fleet of remote flux agents across repos, with
its work state in a board whose implementation is swappable and whose ops are ordinary
policy-gated tools. The reference Program ships with an **offline** end-to-end journey test on
`MemoryBoard` and a stub A2A worker — no credentials, no network.

## Stories

| ID | Story | Notes |
|---|---|---|
| [A-111](../stories/A-111-fleet-coordinator-epic.md) | **Epic** — Fleet coordinator | |
| [A-112](../stories/A-112-per-delivery-bus-isolation.md) | Per-delivery bus isolation in `flux-app` | **blocks the epic**; likely breaking ⇒ MINOR |
| [A-113](../stories/A-113-workboard-port.md) | The `WorkBoard` port: L0 contracts + L5 registration + `MemoryBoard` + contract suite | ⚠ touches `flux-datasource` (protocol line) |
| [A-114](../stories/A-114-markdown-board.md) | `MarkdownBoard` — file-per-item, derived index, IO via `flux-system` | |
| [A-115](../stories/A-115-jira-board.md) | `JiraBoard` over `plugins/jira`, configurable status↔state mapping | `areas: [plugins]` |
| [A-116](../stories/A-116-a2a-outbound-dispatch.md) | `A2aClient::cancel_task` + `A2aSpawner` + `fleet.dispatch`/`.status`/`.cancel` | |
| [A-117](../stories/A-117-coordinator-program.md) | The `coordinator.flux` reference Program + offline end-to-end journey test | |
| [A-118](../stories/A-118-gitlab-board.md) | `GitlabBoard` — proves the port generalizes | deferrable |
| [A-130](../stories/A-130-board-run-state-writeback.md) | Board write-back of `runner` + `task_id` — the op §5 assumed | filed from A-113/A-116 handoffs |

Order: **A-112 first** (nothing works concurrently without it), then A-113 → A-114 / A-115 / A-116
in parallel → A-117 → A-118.

## Release mechanics

- **A-113 touches `flux-datasource`, a protocol-line crate.** It obliges an explicit version
  decision; `scripts/check-crate-versions.sh` in CI is the only thing that catches a miss.
- **A-112 is likely breaking** for `flux-app` embedders ⇒ pre-1.0 SemVer makes that a **MINOR**.

## Deferred to implementation, not settled here

Every runtime claim — bus re-entrancy under concurrent deliveries, A2A task tracking across a
coordinator restart, board contract conformance per backend — is a story's own failing-first test.
This document's job is to make the *shape* reviewable and to pin the claims about the current tree
at `file:line`.
