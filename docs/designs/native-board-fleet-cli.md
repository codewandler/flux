# Native board and fleet CLI

**Status:** accepted by flux-roadmap Decision 0010 · **Epics:**
[A-148](../stories/A-148-first-class-board-epic.md),
[C-239](../stories/C-239-fleet-loop-epic.md) · **CLI contract:**
[C-547](../stories/C-547-versioned-board-fleet-agent-cli-contract.md)

The corrective independent workspace configuration and real roadmap cutover are specified in
[native-workspace-board-cutover.md](native-workspace-board-cutover.md) under Decision 0013/C-588.

## Outcome

`flux board` and `flux fleet` are the supported automation API for a human, Claude or Codex. A
repository supplies stories and declarative configuration, not tracking or coordinator scripts.
Planning state remains in the repository that owns it; fleet execution state is durable runtime
state linked by a concrete board-and-item reference.

## Board model

Every registered board has a stable binding id and three independent properties:

```text
BoardRef      { board: BoardId, item: ItemId }
BoardScope    session(session_id) | repository(repository_id) | workspace(workspace_id)
BoardProfile  general | planning | execution
BoardBackend  session | track | markdown | memory | federated
```

The common item core is identity, title, assignee, dependencies, references, comments and evidence.
General boards use `open|in_progress|blocked|done`. Planning boards use
`backlog|ready|in-progress|blocked|done` and add priority, pillar, design, epic, areas and note.
Execution boards retain the shipped WorkBoard state and runner/task/attempt fields.

A planning board also has a document catalogue. `vision` and `roadmap` are revisioned singletons;
`decision` is a stable collection whose records are `open|decided|superseded`; `design` is a stable
linked collection. An open decision carries its question, options/trade-offs, recommendation and
the exact items it blocks. These documents can reference stories and epics but are not queue items
and never receive a work status. Repository boards normally bind them under `docs/`; a workspace
board may own program-level vision, roadmap and decisions while federating member stories.

Every profile exposes `list`, `get`, `query`, `create`, `transition`, `comment`, `comments` and
`record_evidence`. Planning adds `update`; execution adds `claim`, `record_dispatch` and `reassign`.
The operation set and state machine are fixed per profile and pass one backend-independent contract
suite. The shipped execution profile therefore retains its eleven operations.

The registry resolves every operation through `BoardRef`. A missing board selector is accepted only
when exactly one board supports that operation; two candidates are an ambiguity error listing both.
Subjects are `board:<binding>/item/<id>`. A federated mutation resolves to the concrete member first
and authorizes that member subject, never a broad workspace subject.

Session boards append state transitions to the session event store. Continuation reconstructs the
same board, replay applies recorded events without live writes, and a fork copies the prefix before
diverging. Repository boards are confined to their repository root. Workspace planning boards expose
namespaced member references such as `flux/C-503`, calculate cross-repository dependency readiness
and never copy authoritative story state.

## Agent CLI contract

Every board and fleet command supports human output plus `--output json`; event streams additionally
support NDJSON. Mutations accept a versioned JSON request from `--request FILE|-`,
`--idempotency-key`, `--if-revision` and `--dry-run`. Machine output is one `flux.cli/v1` envelope
with deterministic ordering, request id, revision, data, warnings and a typed error. Exit classes
distinguish invalid input, missing resource, conflict, denial, transient worker failure and failed
validation/gate. Diagnostics never contaminate JSON stdout.

Ergonomic commands are projections over two complete escape hatches:

```text
flux board call BOARD OP --request -
flux fleet call OP --request -
```

`flux board schema` and `flux fleet schema` publish the request, response, enum and capability
schemas. `flux board skill` and `flux fleet skill` render concise Markdown skill bodies by default;
`--output json` returns `{name, description, instructions, cli_schema}`. Each guide contains only
the installed version, safety invariants, discovery path and copyable common calls. It points to
`schema` for detail instead of embedding the full reference. Golden tests parse the Markdown as an
Agent Skill document and execute every shown command against fixtures.

## Planning and fleet CLI

The planning surface covers initialization, discovery, CRUD, transitions, next-item selection,
validation, deterministic rendering, graph queries, statistics, history and reports. The Track
backend preserves YAML frontmatter and the hand-written text outside board markers byte-for-byte.
Compound story, epic, design and done commands are recoverable multi-file changes and expose their
proposed patch in dry-run mode.

`flux board vision show|set`, `flux board roadmap show|set`, and
`flux board decision list|show|create|update|accept|supersede` expose the planning document catalogue;
`flux board design` is the corresponding design-document surface. All accept revisions, dry runs
and JSON. Board checks validate broken document links, duplicate decision ids and a roadmap that
names missing items without rewriting authored prose.

`flux board stats` returns one versioned metric cube. Every count dimension has
`{done, remaining, total, percent}`. Planning boards report epics, stories, optional tasks,
acceptance criteria and headline implementation plus the profile-state histogram; documents report
vision/roadmap presence, decisions by lifecycle and designs. Git-backed boards add canonical commit
totals. Federated scheduled boards add program stories, active milestone lanes and configured waves and
return both per-member and aggregate values. `--history --since YYYY-MM-DD` reconstructs daily
canonical snapshots and reports `scope_added`, `scope_removed` and `completed` deltas. A missing
dimension is `{schema: "absent", done: null, remaining: null, total: null, percent: null}`. HTML/SVG/
TSV reports are pure renderings of this JSON, never independent calculations.

Every fleet has exactly one reserved durable `main` coordinator. All user requirements, tasks and
agent follow-ups enter through its intake; it orchestrates execution against the Board-owned active
roadmap/schedule. It plans
against revisioned goals scoped to values, company, workspace, project and repository. Worker
membership is explicit: `main` admits a worker and records parent, role, session, transport,
capabilities, mode, fences and lease. Merely appearing in configuration or on a transport is not
admission.

A story worker's first model turn is assignment-only. It starts in a worker-specific durable store
without continuation and receives the configured writer contract plus its exact BoardRef, pinned
base, branch and isolated worktree. The main conversation, intake bodies, Fleet-wide revisioned goal
set, other workers and other assignments are not copied into that prompt. Only a later message or
rework addressed to the exact worker may continue its store. Its assigned worktree is the complete
automatic repository scope; sibling repository roots are not mounted into a story-worker process.
Turn receipts expose a bounded
`flux.fleet-context-origin/v1` manifest containing the BoardRef, fresh/continue mode and digests of
the assignment and worker contract; prompt bodies and conversation content are never part of that
manifest.

Worker admission also resolves one normalized capability ceiling. Read-only workers require
`read`; writers require `read`, `edit` and the safe story-sized `git` bundle. Optional `shell`,
language-toolchain and nested-`task` bundles are explicit. The host snapshots that operation set,
mode, writable root, read roots and normalized fences into the durable registration, applies the
same exact operation scope around every fresh or continued model turn, and refuses missing
operations before the model runs. Nested tasks may intersect with that scope but cannot widen it.
Template edits apply only to later admissions; message, restart, resume and rework reconstruct the
existing worker from its snapshot. Status and receipts expose only a bounded
`flux.fleet-capability-set/v1` digest manifest and counts, never the full operation catalogue,
paths, prompt or instruction body.

Worker admission also resolves one explicit versioned agent-loop binding under
[agent-loop-harnesses.md](agent-loop-harnesses.md). General agents may resolve an omitted selector to
the adaptive preset, but Fleet writer/reviewer/decision roles must name a policy-selected profile.
The binding's profile, revision, source digest and entry point are snapshotted beside capabilities;
continuation cannot drift to an edited file or backend default. Task kind is explicit dispatch
metadata and may be mapped by any Board backend without making Board a datasource or loop runner.

Host-observed `SpawnActivity` remains telemetry. A worker-authored progress/yield record uses C-570's
bounded acknowledged channel and cannot mutate Board or Fleet state. C-542/C-571 budget envelopes are
reserved from Fleet through assignment and agent scopes and settled from typed usage; exhaustion is
an inspectable resumable terminal rather than an unstructured failed answer.

`.flux/board.toml` independently declares workspace members, document roots, the active milestone,
ordered program lanes and configured waves. `.flux/fleet.toml` declares main instructions/model,
named reusable agent templates, whether ad-hoc agents are allowed, repository ids/paths, canonical
refs, planning-board bindings, gates, ledger fences and concurrency. Neither configuration file is
runtime state; dispatched wave instances live only in the Fleet state/event journal. The
coordinator may instantiate a template or admit an
ephemeral agent with temporary instructions/model/mode/capabilities/fences at dispatch time; both
paths obey the same limits and can never create a second coordinator. A workspace fleet run selects
from the Board's ordered, active-milestone, dependency-satisfied projection unless the caller
supplies explicit `BoardRef`s.
The durable wave manifest pins source commits, proposed and observed write sets, worktrees,
sessions, attempts, evidence, reviews, gates and local candidate branches.

The host, not a prompt, enforces: at most ten stories per wave; one pinned integration
branch/worktree per repository wave; one child branch/worktree and writer per story, inheriting the
same pinned base; disjoint or serialized write sets; a test-only failing commit before behavior
implementation; a targeted pass before handoff; fresh read-only review; two same-session rework
rounds; dependency-ordered child-commit integration; and one unskippable full gate on each assembled
integration tree. Red preserves the candidate and cannot transition planning items to done. Green
leaves local `fleet/<wave>` branches. Only `flux fleet apply` revalidates and merges them, without
pushing.

The writer workhorse reports `handoff_ready`, after which the host starts a distinct fresh read-only
reviewer under its own reviewer loop. That loop composes the shipped strict-review flow and returns
typed PASS/REWORK/PARK. REWORK enters the original writer's explicit repair entry point; C-245's
two-round host ceiling remains authoritative. The writer never reviews itself.

Open decisions block only their linked work; other ready items continue. Human mode surfaces the
structured choices and recommendation. Auto mode creates a fresh adversarial decision agent with
the applicable values/company/project context, requires it to challenge the recommendation, and
records its chosen outcome and rationale.

Control commands provide durable acknowledgement levels: `accepted` after journalling, `delivered`
after the persistent agent session acknowledges steering, and `completed` at a terminal turn. Status
uses an independent read path while a worker is busy. Its default projection is bounded current
lifecycle metadata and explicit inspect references; it never embeds retained answers, tool events,
intake bodies or repository content. Terminal lifecycle state wins over stale receipt/error text for
active and attention counts. Activity is redacted before persistence.
Targeted inspection applies an explicit item limit and a fixed structural byte budget after
redaction. It prioritizes terminal identity/outcome fields and emits source-referenced omission
records instead of truncating serialized JSON or copying retained evidence into a default view.

## Scriptless parity

The roadmap is the product acceptance fixture. Flux replaces canonical-ref refresh, schedule and
dependency validation, status, worktree audit, worker start/stop/dispatch/follow-up/task/note,
activity, bounded context, progress history and HTML/SVG reporting. Repository-specific build/test
executables remain declared gates; they contain no scheduling or worker-control logic.

Parity is proven side by side before helper removal. The Claude Track commands delegate to Flux and
have no Python fallback. Codex and Claude instructions begin with `flux board skill` or
`flux fleet skill`, then use JSON mode for automation.

## Delivery map

Board wave: C-547 (machine CLI contract), A-134 (registry/profile core), L-130 (declaration), C-548
(session backend), C-549 (Track backend and board CLI), C-550 (federation and schedule).

Fleet wave: C-244 (typed handoff), C-245 (same-session rework), C-242 (integration and explicit
apply), A-117 (durable supervisor and fleet CLI), C-551 (inspection, reporting and roadmap parity).

Fleet dogfood hardening: C-569 (resolved operator-authored loop binding), C-570 (progress/yield),
C-572 (review/repair loops), C-542/C-571 (local and hierarchical budgets), then C-565's five-writer
proof. Postponed C-567 is optional policy/convenience work, not a prerequisite.

Generic task-agent backends and Codex/Claude/Hermes/Pi CLI harness adapters, authenticated remote
A2A fleet members, a polished board/fleet TUI, vendor boards, containers, automatic publication and
automatic worktree deletion are separately contracted follow-up epics rather than V1 dependencies.
