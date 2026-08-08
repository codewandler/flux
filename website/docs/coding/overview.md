---
title: AI-assisted development
description: "Use boards, local Flux sub-agents, and the fleet supervisor as one durable coding workflow."
---

# AI-assisted development

Flux treats AI-assisted development as a durable engineering workflow, not a long prompt. The
**board** says what the project is trying to accomplish and what counts as done. The **fleet** runs
bounded local Flux sub-agents against that work. Repository files, commits, tests, reviews, and the
final gate remain the evidence.

:::info Availability
These pages document the current `main` branch. Native `flux board` and `flux fleet` landed after
v0.55.0 and are included in source installs from `main`; packaged-release users need v0.56.0 or
newer. Confirm an installation with `flux board schema --output json` and
`flux fleet schema --output json`.
:::

```text
vision · roadmap · decisions · designs
                  │
                  ▼
       planning board (authoritative stories)
                  │  BoardRef = board/item
                  ▼
       fleet main coordinator (durable intake + goals)
                     │
                     ▼
          repository wave worktree
          │          │          │
          ▼          ▼          ▼
      sub-agent  sub-agent  sub-agent
       story      story      story
      worktree   worktree   worktree
          └──────────┬──────────┘
                     ▼
       handoff → review/rework → integrate → final gate
                     │
                     ▼
       local candidate → explicit apply → promote onto local main
```

There are two deliberately separate forms of truth:

- Planning truth stays with the board that owns an item. A workspace board can index several
  repositories, but it does not copy their stories into a shadow database.
- Execution truth stays in the fleet manifest and event log: source commit, board revision,
  worktree, session, observed files, test evidence, review, gate, and local candidate branch.

That separation is what lets a coordinator stop and restart without guessing. It can re-read the
planning boards and fold the execution events into the same state it had before.

## Start here as Claude or Codex

Both command families render a small, installed-version guide. The guide stays prompt-sized and
points to the complete machine schema instead of copying it into every context:

```sh
flux board skill
flux fleet skill
flux board schema --output json
flux fleet schema --output json
```

Use human output at a terminal. Use `--output json` from an agent or script. JSON is one stable
`flux.cli/v1` envelope; diagnostics never share stdout with it. Mutations can also use
`--request FILE|-`, `--idempotency-key`, `--if-revision`, and `--dry-run`.

## The three board axes

A board is never identified by its file format alone. Every binding states three independent
choices:

| Axis | Values | Question it answers |
|---|---|---|
| Scope | `session`, `repository`, `workspace` | How long does it live, and where is authority? |
| Profile | `general`, `planning`, `execution` | Which states and operations exist? |
| Backend | `session`, `track`, `markdown`, `memory`, `federated` | How is that contract stored? |

The common item address is `BoardRef { board, item }`, written `board/item` on the CLI. Two
repositories may both have `C-1`; `api/C-1` and `web/C-1` are different work. An omitted board is
accepted only when exactly one compatible binding exists.

Read [Boards](./boards.md) for scope/profile/backend choices, planning documents, Track migration,
statistics, and the complete CLI workflow.

## What the fleet adds

Every fleet has exactly one durable `main` coordinator. Requirements and agent follow-ups enter its
intake; it owns the active schedule and plans against revisioned values/company/workspace/project/
repository goals. It can admit a reusable configured agent template or create an ephemeral agent
with temporary instructions and limits. Neither configuration nor process discovery silently grants
fleet membership.

The fleet selects dependency-satisfied board items, pins one integration worktree per repository
wave, prepares one inheriting story worktree/writer per item, verifies failing-before and
passing-after evidence, obtains a fresh read-only review, allows at most two rework deliveries to
the same session, integrates accepted commits, and runs one final repository gate.

It does **not** silently publish. A green wave leaves a local `fleet/<wave>` candidate, and
`flux fleet apply <wave>` accepts it by pinning that exact commit with an annotated tag — it does not
merge. Writing the canonical branch is `flux fleet promote`, which accumulates each member's accepted
candidates, re-gates them in a throwaway worktree, and advances that member's **local** branch in the
order its dependency graph declares. No Fleet operation pushes, opens a pull request, releases or
deploys.

Read [Fleet and sub-agents](./fleet.md) for configuration, dispatch, acknowledgements, recovery,
inspection, and the publication boundary.

## A normal loop

```sh
# Understand the product contract before changing it.
flux board vision show --output json
flux board roadmap show --output json
flux board decision list --output json
flux board next --limit 3 --output json

# Validate configuration and see the exact wave the supervisor would select.
flux fleet validate --output json
flux fleet goal list --output json
flux fleet schedule --output json

# Route a new requirement through main, then dispatch explicit work or the top eligible wave.
flux fleet start --idempotency-key start-session --output json
flux fleet ingest "Implement the next dependency-satisfied story" --source user --output json
flux fleet run api/C-41 web/C-12 --idempotency-key wave-aug-05 --output json

# Stay responsive while workers run.
flux fleet status --output json
flux fleet inspect activity --limit 100 --output json

# Apply only a recorded green local candidate, then land the accumulation on local main.
flux fleet apply wave-7 --if-revision 18 --idempotency-key apply-wave-7 --output json
flux fleet promote --output json
```

The board's story Goal and Acceptance remain the definition of done. A worker saying “done” is not
evidence; the host-observed commit, write set, test results, review, and final gate are.

## Choosing the smallest useful shape

| Need | Use |
|---|---|
| Scratch checklist for one conversation | session scope + general/planning profile + session backend |
| Stories in one Git repository | repository scope + planning profile + Track backend |
| Existing worker run registry | repository scope + execution profile + Markdown backend |
| One schedule over several repositories | workspace scope + planning profile + federated backend |
| One agent, no dispatch | board commands only |
| Several isolated local writers with restart/review/gate policy | board + fleet |

The backend is an implementation detail after the contract is chosen. A Track board and a session
board with the planning profile present the same planning state machine; a Markdown execution board
does not acquire planning states just because it is also stored in files.
