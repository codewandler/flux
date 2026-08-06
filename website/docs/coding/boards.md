---
title: Boards
description: "Scoped planning and execution boards, planning documents, Track compatibility, exact statistics, and the agent CLI contract."
---

# Boards

A Flux board is a governed collection of work plus an explicit state machine. A planning board also
owns the documents that explain why the work exists. It is separate from a
[datasource](../agent/datasources.md): a datasource is a read surface over knowledge or a system of
record; a board creates, transitions, comments on, and records evidence against work.

:::info Availability
Native `flux board` landed after v0.55.0. It is available in source installs from current `main`;
packaged-release users need v0.56.0 or newer.
:::

## Creating an item commits it

`flux board create` commits the document it writes, path-scoped to that one file, and reports the
commit in its envelope. `--no-commit` opts out.

This is the default rather than a flag because an uncommitted planning item is invisible to the reads
that matter. A federated board resolves each member's items *at that member's `canonical_ref`* — with
`git ls-tree` and `git show`, not from the working tree — so a document that exists only on disk does
not exist as far as the board is concerned. Creating without committing therefore reported success and
changed nothing schedulable.

The commit goes to the current branch, never a side branch: a side branch would reproduce exactly the
invisibility being fixed. Item creation is also the one planning mutation that cannot conflict, because
it only ever adds a path that did not previously exist. Outside a git repository it does nothing, and
mid-merge or mid-rebase it refuses rather than committing into someone else's operation, naming the
path it wrote so nothing is lost.

## Identity and authority

Every operation resolves through a board binding. Every item reference carries both halves:

```json
{ "board": "api", "item": "C-41" }
```

The CLI spelling is `api/C-41`. Permission subjects are equally concrete:
`board:api/item/C-41`. A federated workspace write first resolves `api/C-41` to the `api` member and
authorizes that subject; there is no workspace-wide write shortcut.

Board ids contain ASCII letters, numbers, `-`, `_`, or `.`. Item ids cannot contain `/`. If an
operation could target two boards and no selector was supplied, Flux refuses and lists every
candidate.

## Scope: session, repository, or workspace

### Session

A session board is useful for a temporary decomposition, investigation checklist, or execution plan
that should survive `continue`, replay, and fork but should not become repository files. Mutations
are session events. A continuation reconstructs them; replay consumes the recording without writing
again; a fork inherits the prefix and diverges independently.

Outside a live turn, select the owner explicitly with `--session ID|last`.

### Repository

A repository board is confined to one repository root. The Track planning backend reads YAML
frontmatter under `docs/stories`; the Markdown execution backend keeps its existing TOML-frontmatter
items under `board/items`. These formats remain distinct and are never reinterpreted into one
another.

Read commands do not normalize files, fetch Git refs, clean worktrees, or modify a dirty checkout.

### Workspace

A workspace board federates named repository boards. It returns namespaced references, computes
cross-repository dependency readiness, and detects missing references and cycles, while each story
file remains in its owning repository. The workspace may own program-level vision, roadmap,
decisions, and designs; those do not shadow member documents.

When `.flux/board.toml` declares `default = true`, plain `flux board ...` selects that workspace.
The Board is independent of Fleet: it can list, validate and schedule the program without a
`.flux/fleet.toml`, a running supervisor, or scheduling instructions in README/AGENTS files.

```toml title=".flux/board.toml"
schema = "flux.board-workspace/v1"
id = "product"
default = true
active_milestone = "m1"
vision = "VISION.md"
roadmap = "ROADMAP.md"
decisions = "decisions"
designs = "docs/designs"

[[members]]
id = "api"
root = "../api"
board = "default"
canonical_ref = "origin/main"

[[members]]
id = "web"
root = "../web"
board = "default"
canonical_ref = "origin/main"

[[program]]
id = "api-contract"
item = "api/C-41"
milestone = "m1"
order = 1
depends_on = []
outcome = "Publish the accepted API contract."

[[waves]]
id = "api-m1-1"
state = "active"
repository = "api"
items = ["api/C-41"]
depends_on = []
```

## Domain model

These terms have deliberately narrow meanings:

| Entity | Meaning | Durable authority |
|---|---|---|
| Workspace Board | Cross-repository catalogue and program view; it references member work but does not copy it. | `.flux/board.toml` |
| Member Board | One repository's authoritative work and state machine. | That repository's story files/backend |
| `BoardRef` | Globally unambiguous `MEMBER/ITEM` address, for example `api/C-41`. | Member id plus item id |
| Epic | A larger outcome grouping related stories. It is useful for rollups, but is not itself dispatched. | Member Board epic/design metadata |
| Story | The smallest schedulable implementation contract: Goal, Acceptance, state, dependencies and evidence. | Exactly one member Board |
| Dependency | A prerequisite `BoardRef`. Repository and program dependencies are combined; program configuration cannot remove story dependencies. | Story frontmatter and/or program lane |
| Milestone | A named program horizon. Exactly one workspace milestone is active for `next` and Fleet scheduling. | `active_milestone` and program lanes |
| Program lane | An ordered reference to one story in one milestone. It adds cross-repository ordering/outcome context but has no copied status. | `[[program]]` |
| Configured wave | An ordered, repository-local group of at most ten program stories that may be dispatched together. This is a plan template, not a running job. | `[[waves]]` in Board configuration |
| Decision | A question/outcome record. Only a genuinely `open` structured decision asks for attention and blocks its linked stories. | Workspace or member decision document |
| Design | The accepted technical approach linked from stories/epics. It has no queue state. | Workspace or member design document |

`board next` selects explicit `ready` stories in the active milestone, combines both dependency
sources, and preserves program order. If a program catalogue exists, an unrelated ready member
story is not silently admitted. Fleet consumes this same projection; it does not maintain a second
schedule.

## Profile: general, planning, or execution

All profiles expose the common operations `list`, `get`, `query`, `create`, `transition`, `comment`,
`comments`, and `record_evidence`.

| Profile | States | Additional operations |
|---|---|---|
| General | `open`, `in_progress`, `blocked`, `done` | — |
| Planning | `backlog`, `ready`, `in-progress`, `blocked`, `done` | `update` |
| Execution | `ready`, `claimed`, `in_progress`, `review`, `done`, `blocked`, `failed` | `claim`, `record_dispatch`, `reassign` |

The execution profile keeps the existing eleven-operation WorkBoard surface. Planning does not
pretend a story is a worker run, and execution does not invent priorities or roadmap status.

For the planning profile, the ordinary story path is intentionally small:

```text
backlog ──→ ready ──→ in-progress ──→ done
                         │
                         └──────────→ blocked
                                         │
                                         └──→ ready
```

`ready` is an explicit authorization, not a synonym for “mentioned in a roadmap.” `done` means the
story's Goal and Acceptance are satisfied in the owning repository; a Fleet worker finishing a turn
does not close it by itself.

## Planning documents are not queue items

A planning board has four document families:

- `vision` is a revisioned singleton: the durable destination and principles.
- `roadmap` is a revisioned singleton: sequencing, milestones, and program direction.
- `decision` is a stable collection with `open`, `decided`, and `superseded` lifecycle states.
- `design` is a stable linked collection explaining a technical approach.

They may link stories and epics, but they never receive a story status and never appear in `next`.

```sh
flux board vision show --output json
flux board vision set --file docs/VISION.md --dry-run --output json
flux board roadmap show --output json
flux board decision list --output json
flux board decision show 0010 --output json
flux board decision open D-12 --title "Choose storage" --question "Which store?" \
  --option SQLite --option Postgres --tradeoff "SQLite=local and simple" \
  --tradeoff "Postgres=shared service" --recommended SQLite --blocks C-552 --output json
flux board decision decide D-12 --outcome "SQLite" --rationale "Matches local V1" \
  --if-revision REV --idempotency-key decide-D-12 --output json
flux board design show native-board-fleet-cli --output json
flux board design link native-board-fleet-cli C-552 --output json
```

An open decision is an explicit human-attention queue, not a reason to stop the whole project. It
records the question, structured options/trade-offs, recommendation, and linked stories. Only those
stories become blocked; unrelated ready work remains eligible. Deciding records outcome/rationale
and restores each linked story's prior state and priority.

When `flux tui` is explicitly attached with `--fleet[=ROOT]`, `F2` (or `/board`) opens the same
planning data as bounded native views. Observation stays read-only. Choosing an open decision is the
one Board write available there and requires two Enter presses: one to review the selected option,
one to confirm it. The [TUI operations guide](../agent/tui.md#board-and-fleet-operations) documents
the full navigation and acknowledgement behavior; JSON CLI output remains the automation API.

Before changing a story, an AI coding agent should read the vision, roadmap, applicable accepted
decisions, the story's Goal and Acceptance, and its linked design. `flux board skill` gives the same
instruction in a short Agent Skill document.

## The Track backend

Track repositories work without conversion. Flux reads the existing frontmatter fields (`id`,
`title`, `status`, `priority`, `pillar`, `epic`, `areas`, `note`, and links), preserves story bodies,
and regenerates only the region between:

```markdown
<!-- BEGIN track:board -->
<!-- END track:board -->
```

Text outside those markers is byte-preserved. Rendering is deterministic and idempotent. Ready
stories sort by integer priority then natural id (`C-2` before `C-10`); other sections use natural
id order. Epics use the linked design title and its first paragraph under `## Why` when available.

```sh
flux board init --scaffold
flux board check --output json
flux board render
flux board sync --dry-run --output json
```

### Daily story work

```sh
flux board list --output json # `ls` is an alias
flux board show --output json
flux board items --output json
flux board query --status ready --area flux-cli --output json
flux board next --limit 1 --output json
flux board get C-549 --output json
flux board graph --output json

flux board create --kind story --id C-552 --title "Example" --dry-run --output json
flux board create --kind story --title "Example" --no-commit --output json
flux board update C-552 --priority 51 --if-revision REV --idempotency-key prioritize-C-552 --output json
flux board transition C-552 in-progress --dry-run --output json
flux board start C-552 --if-revision REV --idempotency-key start-C-552 --output json
flux board block C-552 --reason "waiting on API" --output json
flux board unblock C-552 --output json
flux board comment C-552 "review requested" --output json
flux board evidence C-552 "commit/0123456789abcdef" --output json
```

`done` checks every Acceptance checkbox. If any remain, it refuses. An exceptional override must
carry an explicit reason; the reason is recorded. A changelog entry and board regeneration are part
of the same recoverable operation.

```sh
flux board done C-552 --changelog "Add the example" --if-revision REV \
  --idempotency-key done-C-552 --output json
```

## The stable agent API

Human rendering is for terminals. JSON is the agent API:

```json
{
  "schema": "flux.cli/v1",
  "ok": true,
  "request_id": "caller-correlation-id",
  "revision": "9d7d…",
  "data": {},
  "warnings": [],
  "error": null
}
```

Failures use the same envelope and stable exit classes: 2 input/schema, 3 not found, 4 conflict or
precondition, 5 permission, 6 transient worker, and 7 validation/gate. Machine diagnostics do not
leak to stdout.

Use `--if-revision` to prevent a stale agent from overwriting newer state. Use an
`--idempotency-key` so retrying the same mutation returns its original result without applying it
twice. Reusing the key for different input is a conflict.

The ergonomic commands and the complete escape hatch share one schema:

```sh
flux board skill
flux board schema --output json
flux board call stats --request request.json --output json
```

`request.json` is a closed versioned call request such as
`{"schema":"flux.cli/v1","request_id":"stats-1","args":["--history"]}`. The response echoes the
request id. This `args` escape hatch reaches the same validated command implementation; it never
invokes a shell.

## Dependency graph and portable board documents

`graph` returns deterministic item and dependency nodes for schedulers and visualizers. `export`
writes the selected board as a versioned JSON document; `import` accepts that same document through
the destination board's normal validation, revision, idempotency and authorization checks. Import
is not a filesystem-level replacement, so preview it before writing:

```sh
flux board graph --output json
flux board export -o board.json --output json
flux board import board.json --dry-run --output json
flux board import board.json --if-revision REV --idempotency-key import-board --output json
```

## Exact statistics and reports

`flux board stats` returns one metric cube. It is the sole input to JSON, TSV, HTML, and SVG reports;
renderers never recalculate progress independently.

Every ratio uses `{done, remaining, total, percent}` for:

- epics;
- stories;
- optional tasks;
- Acceptance criteria;
- headline implementation.

The cube also includes the profile-state histogram; vision/roadmap presence; open/decided/
superseded decisions; total and story-linked designs; canonical commit facts; and, for a federated
program, program stories, active milestone lanes, configured waves, members, and aggregates. `--history`
reconstructs canonical end-of-day Git snapshots and adds concrete `scope_added`, `scope_removed`,
and `completed` counts plus item ids.

An absent dimension is explicit, never fabricated as zero:

```json
{
  "schema": "absent",
  "done": null,
  "remaining": null,
  "total": null,
  "percent": null
}
```

```sh
flux board stats --history --since 2026-08-01 --output json
flux board report --format tsv -o progress.tsv
flux board report --format html -o progress.html
flux board report --format svg -o progress.svg
```

## Declaring a board in Flux-Lang

Every axis is required. Backend-specific fields are closed and invalid combinations are source-
spanned errors:

```flux
board execution
  scope "repository"
  profile "execution"
  kind "markdown"
  root "./board"
```

The old `datasource kind "board:…"` compatibility spelling is not a second registry. Migrate it to
the first-class declaration; boards and knowledge datasources have different authority and state
contracts.

## Backend summary

| Backend | Typical binding | Durable authority |
|---|---|---|
| `session` | session + general/planning/execution | owning session event stream |
| `track` | repository + planning | YAML-frontmatter stories and authored docs |
| `markdown` | repository + execution | TOML-frontmatter worker items |
| `memory` | any profile in a test/demo | process only |
| `federated` | workspace + planning | member BoardRefs; optional workspace documents |

Next: [Fleet and sub-agents](./fleet.md).
