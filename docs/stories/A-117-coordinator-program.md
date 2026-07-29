---
id: A-117
title: The coordinator.flux reference Program + offline end-to-end journey test
pillar: Agent
status: blocked
priority: 31
epic: fleet-coordinator
design: docs/designs/fleet-coordinator.md
areas: [flux-app, flux-channels]
note: "the epic's headline proof — intake → dispatch → sweep → done against MemoryBoard and a stub A2A worker, no credentials, no network"
---

# The coordinator.flux reference Program + offline end-to-end journey test

## Goal
Ship the coordinator itself: a `.flux` Program that declares the board datasource, the webhook /
schedule / a2a / slack channels, the coordinator agent and the intake / dispatch / sweep journeys —
and prove the whole loop runs. This is the story that turns the epic's parts into a thing you can
run with `flux run coordinator.flux --serve`.

Run state is **not** a new store: `fleet.dispatch` writes the `task_id` and `runner` back into the
board `Item`, so the board *is* the run registry, and the `sweep` journey on a `schedule` channel is
cron-driven reconciliation. Crash recovery is then free — restart, sweep, re-derive.

## Acceptance
- [ ] `coordinator.flux` ships as a reference Program: `datasource board`, `channel jira_hook`
      (webhook), `channel nightly` (schedule), `channel inbox` (a2a), `channel ops` (slack),
      `agent coordinator`, triggers, and the `intake` / `dispatch` / `sweep` journeys.
- [ ] Failing-first test: an **offline** end-to-end cycle against `MemoryBoard` and a stub A2A
      worker — an inbound webhook creates an item, `dispatch` claims it and records `task_id` +
      `runner`, the sweep transitions it to `Done` when the stub reports completion. No credentials,
      no network.
- [ ] Failing-first test: **crash recovery** — a fresh `App` over the same board re-derives every
      in-flight item from board state alone and the sweep resumes; nothing was held in memory that
      mattered.
- [ ] Failing-first test: a sweep over many in-flight items does **not** block inbound webhook
      intake (the payoff for A-112).
- [ ] The Program is documented on the website's app/channels pages, and any new config keys are
      covered by the existing config-completeness assertions.

## Progress
- **Blocked at pickup (base `6418ef81`).** No code written: the story cannot be implemented as
  specified without four decisions that belong to the epic, not to this story. Each was verified
  against the tree rather than inferred; `path:line` below is at the base commit.

### B1 — the board cannot hold run state, so it cannot be the run registry
`WorkBoard` (`crates/flux-capabilities/src/datasource/board.rs:83-119`) has exactly six methods —
`list` / `get` / `create` / `transition` / `claim` / `comment`. **None of them can write
`Item::runner` or `Item::task_id`.** `ItemDraft` (`crates/flux-datasource/src/board.rs:243-256`)
carries only `title` / `assignee` / `depends_on` / `repo`, and `MemoryBoard::create` hardcodes
`runner: None, task_id: None` (`crates/flux-capabilities/src/datasource/memory_board.rs:160-161`).
The fields are *read* by `render_full` (`board.rs:512-519`) and by nothing else.

A-113's own doc-comment says these are "written later by dispatch and execution"
(`flux-datasource/src/board.rs:241`) — but that write path was never landed. So the Goal's premise
("`fleet.dispatch` writes the `task_id` and `runner` back into the board `Item`") and Acceptance 2
("`dispatch` claims it and records `task_id` + `runner`") have **no port method to call**, and
Acceptance 3 (crash recovery re-derives in-flight items *from board state alone*) has nothing
persisted to re-derive from.

Unblocking needs a decision, not a guess: extend `WorkBoard` with a dispatch-recording method (a
7th generated op, which changes `OPERATIONS: [&str; 6]`, the contract suite, and every backend), or
change `fleet.dispatch` to take a board handle. Both land outside this story's `areas` and both
collide with A-114/A-115.

### B2 — nothing registers the `fleet.*` ops, so no Program can call them
`FleetDispatchTool` / `FleetStatusTool` / `FleetCancelTool` / `A2aSpawner` are constructed **nowhere**
in the workspace outside their own module; the only other mention is the re-export at
`crates/flux-orchestrate/src/lib.rs:18`. `flux-app` still builds a `LocalSpawner`
(`crates/flux-app/src/app.rs:3497`). A-116 shipped the ops but not their host wiring, so a journey
body calling `fleet.dispatch` resolves to no registered op today.

### B3 — a socket-free A2A worker stub does not exist
`A2aClient` hardwires its transport (`http: reqwest::Client` built by `reqwest::Client::new()`,
`crates/flux-a2a/src/client.rs:44,70`); there is no injectable-transport seam — only `with_token`,
`with_header`, `with_rpc_url`. `flux_a2a::server::dispatch` is socket-free but implements
`message/send` only and explicitly classifies `tasks/get` / `tasks/cancel` as unsupported
(`crates/flux-a2a/src/server.rs:177,195-207`), which `fleet.rs:16-18` already calls out. Every
existing `fleet.*` test therefore binds a real loopback listener
(`crates/flux-orchestrate/src/fleet.rs:518-556`).

So "stub A2A worker, no network" is satisfiable only by (a) a loopback TCP listener, or (b) stubbing
at the op boundary — injecting test `fleet.*` tools via `App::try_with_tools`. Which one counts is a
review criterion this story must state before the test is written, because they prove different things.

### B4 — a Program `datasource` decl cannot bind to a `WorkBoard`
`datasource board { kind = ... }` is routed through `build_datasources`
(`crates/flux-cli/src/execution.rs:178-228`), which knows exactly two kinds — `markdown` and
`openapi` — and builds a **knowledge** `DatasourceBackend`. There is no seam binding a decl to a
`WorkBoard`. `kind "jira"` is a hard error today; `kind "markdown"` would silently ingest a docs
directory as knowledge rather than construct a `MarkdownBoard`. `flux run coordinator.flux --serve`
cannot construct the board the Goal names.

### B5 — the examples sweep rejects an agent-bound trigger that the runtime accepts
Not a blocker, but it will bite whoever writes the Program, so it is recorded here.

`examples/` is swept by `every_example_validates_against_its_form_appropriate_gate`
(`crates/flux-eval/tests/examples_validate.rs:101-165`; note it enumerates **only** the repo-root
`examples/`, so `crates/flux-app/examples/` is not covered). For a `Module::Program` it runs
`validate_program_structure` (`examples_validate.rs:88-99`), which asserts
`program.flow_named(&t.run).is_some()` for **every** trigger, unconditionally.

An **agent-bound** trigger (`agent coordinator`, no `run`) parses with `run == ""` — the parser
defaults it (`crates/flux-lang/src/cst_decode.rs:1555`) and requires only that one of `run`/`agent`
is present (`:1556-1561`). `flow_named("")` is `None`, so the sweep fails it. `flux-app`'s own
`Engine::validate` **does** exempt agent-bound triggers (`crates/flux-app/src/app.rs:1003-1017`).

So the two validators disagree, and A-117 walks straight into it: Acceptance 1 wants `channel inbox`
(a2a) plus `agent coordinator`, which is the agent-bound shape. Either give every trigger in
`coordinator.flux` an explicit `run`, or fix `validate_program_structure` to mirror the runtime's
exemption. The second is the better fix — the sweep being *stricter* than the runtime means a valid
Program cannot be shipped as an example.

### Suggested split
Acceptance 4 (a sweep over many in-flight items does not block webhook intake) is the one item that
is implementable today — it needs only `board.list` over `MemoryBoard` plus the A-112 rendezvous
pattern at `crates/flux-app/tests/integration.rs:770-835`. It could be carved off and landed while
B1/B2/B4 are resolved as their own stories.

## Notes
- Design: [fleet-coordinator.md §5, §8](../designs/fleet-coordinator.md).
- Depends on A-112 (concurrency), A-113 (+ at least one real backend, A-114 or A-115), and A-116
  (dispatch).
