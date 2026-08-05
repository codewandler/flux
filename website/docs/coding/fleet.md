---
title: Fleet and local sub-agents
description: "Configure, dispatch, inspect, recover, review, gate, and explicitly apply bounded local coding waves."
---

# Fleet and local sub-agents

The Flux fleet is a durable supervisor connecting planning [`BoardRef`s](./boards.md) to isolated
local Flux sub-agent sessions. It replaces coordinator socket clients, terminal scraping, Python
board generators, status collectors, and hand-built progress reports with one versioned CLI.

The fleet is local in V1. It does not require remote A2A workers, containers, automatic publication,
or automatic worktree deletion.

## Configure the workspace

`flux fleet init` creates a closed `.flux/fleet.toml` scaffold and durable `.flux/fleet/` state.
Limits default to three concurrent workers, ten stories per wave, and two rework deliveries.

```sh
flux fleet init --max-workers 3 --max-wave 10 --max-rework 2
flux fleet doctor --output json
flux fleet validate --output json
```

A workspace declares repository identity, root, canonical ref, planning board, final gate, write
fences, concurrency, and tranche/wave grouping. A representative configuration is:

```toml
schema = "flux.fleet/v1"
max_workers = 3
max_wave = 10
max_rework = 2

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

Validation rejects duplicate ids, overlapping roots, missing boards, invalid refs, dependency
cycles, a wave over ten, and unsupported fields. Refresh and other read commands report dirty,
stale, or diverged checkouts without fetching or modifying them.

## Schedule and dispatch

The default scheduler selects the highest-priority dependency-satisfied wave. Explicit `BOARD/ITEM`
arguments go through the same checks.

```sh
flux fleet refresh --output json
flux fleet schedule --output json
flux fleet start --idempotency-key fleet-start --output json

# Let Flux choose the top eligible wave.
flux fleet run --idempotency-key next-wave --output json

# Or name a bounded set explicitly.
flux fleet run api/C-41 web/C-12 --idempotency-key aug-05-wave --output json
```

One wave contains at most ten stories. Each writing story gets exactly one writer, one fresh
isolated worktree, one persistent Flux session, and one story-sized commit. Overlapping or uncertain
write sets serialize. Read-only maintenance tasks are the default and need no story worktree:

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

## Review and bounded rework

A fresh read-only reviewer inspects the exact handoff commit. Findings are structured path/line,
command-output, or invariant records with reviewer identity. A REWORK decision is delivered back to
the same persistent worker session, preserving its context.

The host allows two rework deliveries. A third request parks the item with unresolved findings; a
board transition, cancellation, restart, or new CLI call cannot reset the counter.

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

```sh
flux fleet status --output json
flux fleet inspect integration wave-7 --output json
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
flux fleet dashboard --output json
```

Events are redacted before persistence, not merely at display time. The corpus covers credentials,
`.env`, key files, model commentary, commands, diffs, and JSON fields. Follow mode emits NDJSON so
an agent does not parse terminal decoration.

The paired board commands replace progress collectors and report generators:

```sh
flux board stats --history --output json
flux board report --format html -o progress.html
flux board report --format svg -o progress.svg
```

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

