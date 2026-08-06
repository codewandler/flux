---
title: Fleet and local sub-agents
description: "Configure, dispatch, inspect, recover, review, gate, and explicitly apply bounded local coding waves."
---

# Fleet and local sub-agents

The Flux fleet is a durable supervisor connecting planning [`BoardRef`s](./boards.md) to isolated
local Flux sub-agent sessions. It replaces coordinator socket clients, terminal scraping, Python
board generators, status collectors, and hand-built progress reports with one versioned CLI.

:::info Availability
Native `flux fleet` landed after v0.55.0. It is available in source installs from current `main`;
packaged-release users need v0.56.0 or newer.
:::

The fleet is local in V1. It does not require remote A2A workers, containers, automatic publication,
or automatic worktree deletion.

## Domain model and ownership

Fleet executes work selected by the [Board domain model](./boards.md#domain-model). It does not own
epics, story status, milestones or the program schedule.

| Entity | Meaning | Durable authority |
|---|---|---|
| Fleet | One local execution supervisor rooted at a workspace. | Fleet config plus runtime journal |
| Main coordinator | The single reserved agent that accepts requirements and orchestrates dispatch. It selects from the Board; it does not replace Board authority. | Fleet runtime state |
| Worker | One admitted sub-agent with one role, capability ceiling, persistent session and bounded assignment. In the normal story path, worker and sub-agent are 1:1. | Fleet admission/runtime state |
| Configured wave | Board-owned repository-local dispatch template. It has no worker, worktree or runtime lifecycle. | `.flux/board.toml` |
| Dispatched wave instance | One pinned execution of selected BoardRefs, with integration bases, workers, receipts, reviews, gate and apply status. | `.flux/fleet/state.json` and events |
| Handoff | Typed candidate result for one story: exact commit, write set and test evidence. It is not completion. | Fleet runtime state/events |
| Review | Fresh read-only assessment of the exact candidate commit and story contract. A result is PASS, REWORK or PARK. | Fleet runtime state/events |
| Gate | Repository command run against the assembled wave candidate. Green makes the wave apply-eligible; it does not publish it. | Fleet receipt/runtime state |
| Apply | Explicit local integration of a green candidate into the configured checkout. | Git plus Fleet runtime state |
| Release/deploy | Separate publication boundary after apply. Fleet never implies either from a green gate. | Release/deployment system |

The word “wave” should therefore be qualified when it matters: a **configured wave** is planning
configuration; a **dispatched wave instance** is mutable execution state.

## Configure the workspace

Configure the cross-repository program in `.flux/board.toml` first; see the
[workspace Board example](./boards.md#workspace). `flux board check --output json` works without
Fleet. `flux fleet init` then creates a closed execution-policy scaffold and durable runtime state.
Limits default to three concurrent workers, ten stories per wave, and two rework deliveries.

```sh
flux fleet init --max-workers 3 --max-wave 10 --max-rework 2
flux fleet doctor --output json
flux fleet validate --output json
```

Fleet configuration declares execution identity, repository roots, canonical refs, final gates,
write fences, concurrency and worker admission. Milestones, program lanes and configured waves do
not belong here. A representative execution configuration is:

```toml
schema = "flux.fleet/v1"
max_workers = 3
max_wave = 10
max_rework = 2
decision_mode = "human" # or "auto"
allow_ad_hoc_agents = true
worktree_root = ".flux/fleet/worktrees"

[main]
instructions = ".flux/fleet/main.md"
model = "codex/gpt-5.6-sol"

[[agent_templates]]
id = "story-worker"
role = "writer"
instructions = ".flux/fleet/agents/story-worker.md"
model = "codex/gpt-5.6-sol"
mode = "write"
capabilities = ["read", "edit", "git", "shell", "rust"]
fences = [".flux/fleet/**"]
max_instances = 3

[[repositories]]
id = "api"
root = "../api"
canonical_ref = "origin/main"
board = "product"
gate = ["cargo", "test", "--workspace"]

[[repositories]]
id = "web"
root = "../web"
canonical_ref = "origin/main"
board = "product"
gate = ["npm", "test"]
```

Instruction paths are confined under the fleet root. Validation rejects duplicate/reserved ids,
another coordinator role, invalid instance limits, overlapping roots, missing boards, invalid refs,
and unsupported fields. Board validation separately rejects program dependency cycles,
cross-repository configured waves, or a wave over ten. Refresh and other read commands report
dirty, stale, or diverged checkouts without fetching or modifying them.

### Configuration is not state

The durable files have disjoint write ownership:

| Path | Contains | Mutated by runtime operations? |
|---|---|---|
| `.flux/board.toml` | Board members, documents, active milestone, program lanes, configured waves | No |
| `.flux/fleet.toml` | Worker templates, models, capabilities, gates, fences, concurrency, worktree policy | No |
| `.flux/fleet/state.json` | Coordinator/workers and dispatched wave instances | Yes |
| `.flux/fleet/events.ndjson` | Append-only lifecycle receipts and evidence | Yes |

Read surfaces may join these sources in one response. Fleet state stores BoardRefs and the minimum
pinned dispatch snapshot needed for reproducibility; it is not a copied or mutable program schedule.

## Capabilities are an admission ceiling

Each template must declare the authority its workers need. A read-only template requires `read`; a
writing template requires `read`, `edit`, and the safe story-sized `git` bundle. Add only the
optional bundles the work requires: `shell` for arbitrary guarded processes, `rust`, `node`, `go`,
`python`, or `make` for a native toolchain, and `task` for nested sub-agent delegation. Shell and
toolchain processes still run inside bounded-autonomy's fail-closed workspace sandbox with the
network closed. There is no implicit network capability.

Fleet validates the declared names before dispatch, expands them to one exact host-owned operation
set, and records a bounded `flux.fleet-capability-set/v1` digest in status and turn receipts. The
same mode, operations, writable root, read roots, and fences survive message delivery, process
restart, resume, and rework. Editing a template affects only workers admitted afterward; it never
widens an existing worker. Admit a new worker explicitly when the required authority changes.

## One main coordinator, goals, and intake

Every fleet has exactly one reserved `main` coordinator. It is the only agent that owns requirement
intake and execution orchestration. The Board owns roadmap and scheduling truth. All user tasks and worker follow-ups route through it;
worker records carry `parent: main` and no template or ad-hoc request may use the coordinator role.

For an interactive operator surface, explicitly attach the TUI to this coordinator:

```sh
flux fleet start --output json
flux tui --fleet               # current directory is the Fleet root
flux tui --fleet=../roadmap    # or name the Fleet root
```

This opens the exact durable `main` session and keeps the conversation primary. `F2`, `/fleet`, or
`/board` opens bounded native Board, worker, decision, failure, planning-document, and exact-stats
views; wide terminals also show an attention rail. Typed requirements are journalled through
accepted, delivered, and completed/failed acknowledgement states. A stopped Fleet is observable but
cannot accept input. See the [TUI guide](../agent/tui.md#board-and-fleet-operations) for navigation,
decision confirmation, restart behavior, and the deliberately narrow mutation boundary.

The main agent plans against revisioned context rather than an untracked system prompt:

```sh
flux fleet goal set values engineering "Prefer evidence and reversible changes" --output json
flux fleet goal set company product "Make Flux the agent automation substrate" --output json
flux fleet goal set project flux "Replace repository helper scripts" --output json
flux fleet goal list --output json
flux fleet ingest "Add a cross-repository board" --source user --output json
flux fleet ingest "Reviewer found a stale gate" --source agent --from reviewer-2 --output json
```

Reusable roles are admitted from templates. The coordinator can also create a temporary specialist
on the fly; both receive the same durable registration, capability/mode/fence validation, limits and
lease, and neither path can create a second main agent:

```sh
flux fleet spawn --template story-worker --item api/C-41 --name writer-C-41 --output json
flux fleet spawn --role critic --instructions "Challenge D-12 against project goals" \
  --mode read-only --name critic-D-12 --output json
```

Configuration makes an agent available for admission; it does not silently register a live member.
Future CLI-harness and remote A2A task backends use this same admission record without changing who
owns the roadmap.

## Decisions without stopping autopilot

`flux fleet decisions` aggregates open board decisions. Human mode prints each question, structured
options/trade-offs and recommendation so the operator usually only needs to pick. Linked stories
stay blocked, while unrelated eligible work continues.

```sh
flux fleet decisions --output json
flux fleet decisions --auto --output json
```

Auto mode admits a fresh adversarial decision agent. It sees the applicable values/company/project
goals, must challenge the proposing agent's recommendation, and records a rationale. It does not
reuse the proposer context or turn every worker into a coordinator.

## Schedule and dispatch

The scheduler reads the Board's active-milestone projection and configured waves. It preserves
program order, combines story/program dependencies, and never falls back to an unrelated ready
story when the workspace has a program catalogue. Explicit `BOARD/ITEM` arguments go through the
same authority and readiness checks.

Before an item is dispatchable, every boundary in this chain must hold:

```text
repository story exists
          │
          ▼
Goal and Acceptance define done
          │
          ▼
workspace schedule authorizes the BoardRef
          │
          ▼
dependencies are satisfied
          │
          ▼
story is ready on the pinned canonical ref
          │
          ▼
worker capacity + write-set independence are valid
          │
          ▼
dispatch
```

Fleet refuses rather than borrowing a dirty checkout, guessing a missing contract, or widening a
worker's scope to make the schedule fit.

```sh
flux fleet refresh --output json
flux fleet schedule --output json
flux fleet start --idempotency-key fleet-start --output json

# Let Flux choose the top eligible wave.
flux fleet run --idempotency-key next-wave --output json

# Or name a bounded set explicitly.
flux fleet run api/C-41 web/C-12 --idempotency-key aug-05-wave --output json
```

One configured wave contains at most ten stories. Dispatch creates a distinct wave instance. For
each repository, `run` pins the canonical commit and
creates one integration branch/worktree. Every writing story receives one child branch/worktree
from that exact base, one writer, one persistent Flux session and story-sized commits:

```text
canonical base
└── wave integration worktree
    ├── story C-41 worktree
    └── story C-42 worktree
```

Targeted/cheap checks run in story children. Accepted exact commits integrate into the wave in
dependency order; the configured full gate runs once only after the assembled tree is final.
Overlapping or uncertain write sets serialize or refuse before integration. Read-only maintenance
tasks are the default and need no story worktree:

```sh
flux fleet task api "audit the next ready contract" --mode read-only --output json
flux fleet task api/C-41 "implement the accepted story" --mode write --output json
```

## The typed handoff

A worker result is not parsed from prose. Its `FleetHandoff` names:

- the exact `BoardRef`, worker, session, worktree, and branch;
- an exact commit, never only a branch name;
- the normalized approved and observed write sets;
- test argv;
- host-observed failing-before and passing-after evidence;
- a short summary.

For behavioral work, the host runs the test before implementation and requires it to fail, then
runs the same typed argv at the returned commit and requires it to pass. Documentation-only work
must declare validation argv and a reason no failing test applies. The host compares the commit diff
with the approved write set; a worker cannot widen its own fence by claiming it did not.

Malformed or contradictory handoffs are refusals. Cancellation or a crash leaves the worktree,
commit, event log, and evidence intact for inspection and resume.

The ergonomic command is fully typed; `--test-arg` is repeated so no shell string is parsed:

```sh
flux fleet handoff wave-7 api/C-41 --commit FULL_SHA \
  --write-set crates/api/src/lib.rs --write-set crates/api/tests/contract.rs \
  --test-arg cargo --test-arg test --test-arg=-p --test-arg api \
  --failing-before --passing-after --summary "Implemented the accepted contract" --output json
```

## Review and bounded rework

A fresh read-only reviewer inspects the exact handoff commit. Findings are structured path/line,
command-output, or invariant records with reviewer identity. A REWORK decision is delivered back to
the same persistent worker session, preserving its context.

```text
assignment
    │
    ▼
isolated writer session + story worktree
    │
    ▼
failing-first evidence → implementation → targeted checks
    │
    ▼
exact commit + typed handoff
    │
    ▼
fresh read-only review
    ├── ACCEPT ──→ dependency-order integration
    └── REWORK ──→ same writer session (at most two deliveries)
                         │
                         └── third failure ──→ parked
```

The reviewer is not a second writer, and the original writer does not review itself. Review changes
the next execution step; the host-observed commit, write set, commands, and evidence remain the
facts.

The host allows two rework deliveries. A third request parks the item with unresolved findings; a
board transition, cancellation, restart, or new CLI call cannot reset the counter.

```sh
flux fleet rework wave-7 api/C-41 --reviewer reviewer-2 --reviewed-commit FULL_SHA \
  --path 'crates/api/src/lib.rs:91:Preserve the prior error class' \
  --invariant 'No partial board evidence on refusal' --output json
```

`message` uses the same acknowledged steering channel:

```sh
flux fleet message worker-1 "address review r-17" --wait accepted --output json
flux fleet message worker-1 "address review r-17" --wait delivered --output json
flux fleet message worker-1 "address review r-17" --wait completed --output json
```

`accepted` means durably journalled, `delivered` means the persistent session acknowledged the
steer, and `completed` means that follow-up turn reached a terminal result. Idempotent replay does
not consume another rework attempt.

## Integration, the final gate, and apply

Accepted story commits are integrated in dependency order onto one local candidate. Inputs carry
BoardRef, writer/worktree identity, exact commit, write sets, and targeted evidence. Duplicate
stories/writers, unsafe overlap, and more than ten inputs refuse before integration.

After the final accepted commit, the declared full repository gate runs exactly once. A missing or
unrunnable gate is red. A conflict or red gate records the exact candidate and preserves its history;
planning stories do not become done.

A green gate records a local `fleet/<wave>` branch as apply-eligible. Nothing is published yet:

```text
targeted checks green
          │
          ▼
story commit accepted by review
          │
          ▼
wave integration gate green
          │
          ▼
local candidate recorded
          │
          ▼
explicit local apply
          │
          ▼
canonical story status done
          │
          ▼
push / release / deployment / milestone exit
        remain separate operator actions
```

```sh
flux fleet status --output json
flux fleet inspect integration wave-7 --output json
flux fleet integrate wave-7 --if-revision 17 --idempotency-key integrate-wave-7 --output json
flux fleet apply wave-7 --if-revision 18 --idempotency-key apply-wave-7 --output json
```

`apply` revalidates the base, board revisions, gate record, and repository cleanliness, then merges
locally in repository order. It never pushes, opens a pull request, tags, releases, deploys, or
deletes a worktree. Those are separate operator decisions.

## Restart and resume

The durable state/event log pins board revisions, source commits, write sets, worktrees, sessions,
attempts, handoffs, reviews, gates, and candidates. On restart the supervisor folds those records
and execution boards; process memory is not authoritative.

```sh
flux fleet status --output json
flux fleet resume --output json
flux fleet resume worker-1 --output json
flux fleet cancel worker-1 --output json
flux fleet stop --output json
```

Status has an independent read lane, so a busy or stuck worker cannot make fleet inspection hang.

`status` and `dashboard` are bounded projections, not a dump of durable state. They report main
state, active and attention worker counts, wave and item state, exact board references,
repositories, current sessions, the last transition or error summary and the current revision, and
they stay inside a fixed byte budget however much turn history the fleet has retained. Answers, tool
events, diffs, instruction bodies and repository contents are never copied into them; anything the
budget drops is reported as payload-free omission metadata, and the human form names the next useful
inspection or recovery command. A worker counts as active only while its recorded transition says a
turn is in flight, so a completed, failed, cancelled or interrupted process is never active merely
because an old receipt still says `working`.

## Bounded inspection replaces helper scripts

Every inspect command has a stable JSON form and an explicit bound:

```sh
flux fleet agents --output json
flux fleet worktrees --output json
flux fleet events --limit 200 --output json
flux fleet events --follow --output ndjson
flux fleet logs worker-1 --limit 200 --output json
flux fleet inspect snapshot --limit 100 --output json
flux fleet inspect wave wave-7 --limit 100 --output json
flux fleet inspect worker worker-1 --limit 100 --output json
flux fleet inspect result api/C-41 --limit 100 --output json
flux fleet inspect activity --limit 100 --output json
flux fleet inspect worktree worker-1 --limit 100 --output json
flux fleet inspect integration wave-7 --limit 100 --output json
flux fleet inspect source api --limit 100 --output json
flux fleet inspect search C-41 --limit 100 --output json
flux fleet inspect story api/C-41 --limit 100 --output json
flux fleet inspect pull-request wave-7 --limit 100 --output json
flux fleet note "Candidate preserved while CI is unavailable" --output json
flux fleet dashboard --output json
```

Events are redacted before persistence, not merely at display time. The corpus covers credentials,
`.env`, key files, model commentary, commands, diffs, and JSON fields. Follow mode emits NDJSON so
an agent does not parse terminal decoration.

Fleet also bounds evidence before the supervisor retains it. Adaptive tool results larger than
64 KiB become payload-free omission records; accumulated adaptive message history refuses above
512 KiB and a complete provider request refuses above 1 MiB. Child NDJSON lines are limited to
240 KiB (including the newline), below the 256 KiB parser boundary. An oversized nonterminal event
becomes `event_omitted`; an oversized `turn_end` keeps its session, outcome, usage and cost while its
answer/error payload becomes `payload_omitted`. `turn.budget`, `model.call` and the Fleet receipt's
`stream_budget` report only the affected domain, byte count, limit and post-redaction digest—never
the omitted repository content. Use `inspect worker` or `inspect result` to correlate that digest;
do not expect `status` to reproduce an omitted payload.

The paired board commands replace progress collectors and report generators:

```sh
flux board stats --history --output json
flux board report --format html -o progress.html
flux board report --format svg -o progress.svg
```

`note` appends redacted coordinator context to the same durable event stream. It is for facts that
must survive a restart but are not a worker instruction; use acknowledged `message` for steering.

## The stable agent API

Fleet uses the same `flux.cli/v1` envelope, revision preconditions, idempotency keys and closed
request format as board. `schema` is the complete installed-version contract, while `call` is the
automation escape hatch into the same validated implementation:

```sh
flux fleet skill
flux fleet schema --output json
flux fleet call status --request status-request.json --output json
```

For `call`, the request document supplies only the operation arguments, for example
`{"schema":"flux.cli/v1","request_id":"status-1","args":[]}`. It never invokes a shell or bypasses
fleet state validation.

## Safe agent operating loop

An AI coordinator can keep only this compact loop in context:

1. Run `flux fleet schema --output json` once for the installed version.
2. Run `validate`, `schedule`, and `status` before dispatch.
3. Dispatch only dependency-satisfied, explicitly authorized board items.
4. Inspect status/activity through bounded JSON; use acknowledged `message` for follow-ups.
5. Resume durable state after interruption; do not reconstruct it from terminal history.
6. Apply only a recorded green candidate, with its current revision.
7. Treat push, release, deploy, and cleanup as separate operator actions.

`flux fleet skill` renders that loop as a concise Agent Skill for Claude, Codex, and other harnesses.

## Deliberate follow-ups

V1 workers are native local Flux sub-agents. Three later epics preserve clean boundaries:

- a generic task-agent backend plus local Codex, Claude, Hermes and Pi CLI adapters;
- authenticated invitation/hello/admission/lease for remote A2A members, followed by an A2A task
  backend; and
- a polished TUI centered on the main coordinator conversation, with read-only worker-channel peeks
  and native board, decision and statistics views.

Those transports and views reuse the same BoardRefs, admission records, evidence, decisions and
publication fence. They are not hidden capabilities of the local V1 and are not required to call
the board/fleet CLI from Claude or Codex today.
