---
title: Boards
description: "Scoped planning and execution boards, planning documents, Track compatibility, exact statistics, and the agent CLI contract."
---

# Boards

A Flux board is a governed collection of work plus an explicit state machine. A planning board also
owns the documents that explain why the work exists. It is separate from a
[datasource](../agent/datasources.md): a datasource is a read surface over knowledge or a system of
record; a board creates, transitions, comments on, and records evidence against work.

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

## Planning documents are not queue items

A planning board has four document families:

- `vision` is a revisioned singleton: the durable destination and principles.
- `roadmap` is a revisioned singleton: sequencing, milestones, and program direction.
- `decision` is a stable collection with `proposed`, `accepted`, and `superseded` lifecycle states.
- `design` is a stable linked collection explaining a technical approach.

They may link stories and epics, but they never receive a story status and never appear in `next`.

```sh
flux board vision show --output json
flux board vision set --file docs/VISION.md --dry-run --output json
flux board roadmap show --output json
flux board decision list --output json
flux board decision show 0010 --output json
flux board decision accept 0010 --if-revision REV --idempotency-key accept-0010 --output json
flux board design show native-board-fleet-cli --output json
```

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
flux board ls --output json
flux board items --output json
flux board query --status ready --area flux-cli --output json
flux board next --limit 1 --output json
flux board get C-549 --output json

flux board create --kind story --id C-552 --title "Example" --dry-run --output json
flux board update C-552 --priority 51 --if-revision REV --idempotency-key prioritize-C-552 --output json
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
  "request_id": null,
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
flux board schema --output json
flux board call default stats --request request.json --output json
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

The cube also includes the profile-state histogram; vision/roadmap presence; decisions by lifecycle;
design count; canonical commit facts; and, for a federated program, program stories, tranche lanes,
waves, groups, members, and aggregates. `--history` adds daily `scope_added`, `scope_removed`, and
`completed` deltas.

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
board product
  scope "repository"
  profile "planning"
  kind "track"
  root "."
  vision "docs/VISION.md"
  roadmap "docs/ROADMAP.md"
  decisions "docs/decisions"
  designs "docs/designs"
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

