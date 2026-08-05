# Design — explicit agent-loop harnesses, progress and hierarchical budgets

**Status:** proposed, active Fleet stop-line · **Epic:**
[C-568](../stories/C-568-explicit-agent-loop-harnesses-epic.md) · **Stories:**
[C-569](../stories/C-569-resolve-loop-binding-at-every-agent-start.md),
[C-567](../stories/C-567-run-codex-fleet-writers-as-fresh-workhorses.md),
[C-570](../stories/C-570-agent-progress-and-cooperative-yield.md),
[C-571](../stories/C-571-hierarchical-fleet-budget-ledger.md),
[C-572](../stories/C-572-fleet-review-and-rework-loops.md),
[C-573](../stories/C-573-metrics-driven-fleet-policy-controller.md),
[C-583](../stories/C-583-live-fleet-capacity-control.md)

## Outcome

Every agent starts under a resolved, versioned loop harness that says how it behaves. The ordinary
Flux default remains the authored adaptive loop, but `None` never reaches a running agent: the start
boundary resolves it to `builtin:adaptive@1` and records that exact binding. A Fleet writer instead
uses a purpose-built workhorse loop; a fresh reviewer uses a reviewer loop; a decision or research
task may use another declared profile.

The loop is independent of where the agent executes. Native Flux can execute arbitrary validated
Flux-Lang loops. A task-agent backend advertises which loop forms and reporting semantics it can
honestly implement and refuses an unsupported binding before admission. It never silently falls
back to its own intrinsic default.

This fixes the 2026-08-05 five-writer dogfood failure at the right layer. All five workers started
fresh and inside their admitted capability ceilings, but the general adaptive
`detect_intent -> explore` loop repeatedly rediscovered their repositories until the 50-call or
512-KiB history ceiling stopped them. Fresh context and a safe sandbox were necessary; neither
made the general explorer an implementation workhorse.

## Existing contracts and the boundary between them

| Existing contract | What it already provides | What remains here |
|---|---|---|
| `AgentSpec::agent_loop`, role `loop`, `flux run --loop` | Native authored loop selection | Resolve and snapshot a binding on every start path, including task and Fleet starts |
| `SpawnActivity` / `subagent.activity` | Correlated host-observed planning, tool and terminal telemetry | Child-authored, acknowledged progress and cooperative yield |
| `await`, checkpoints and resumable journeys | Program-declared durable suspension | A standard agent report/yield contract tied to the active assignment and loop cursor |
| A-140/A-141 run control | Operator-initiated pause at a safe boundary | Worker-initiated yield; it is not pause or cancellation |
| C-244/C-245 Fleet handoff and rework | Host-verified handoff, typed findings and two reworks | Make work, review and repair behavior explicit loop profiles |
| `strict_review.flux` | Read-only typed multi-reviewer protocol | Reuse it inside the Fleet reviewer loop rather than inventing review prose |
| `SpawnLimits`, `ResourceLimits`, adaptive limits | Several per-turn/per-agent ceilings | One hierarchical allocation and settlement model across Fleet, wave, task and agent |
| C-130 and C-542 | Monetary enforcement; live time/token limits | Reuse their accounting vocabulary and extend it to Fleet hierarchy |
| C-552/C-553 task-agent backends | Generic lifecycle plus future CLI adapters | Make loop/report/yield/budget support part of backend discovery and admission |

The older agent-loop visibility stories remain views over loop events. C-543/C-544 remain the
interactive selector and authoring surfaces. This epic gives those surfaces a stable binding to
select and display; it does not absorb their TUI work.

## One explicit start contract

Every constructor that can make an agent runnable resolves an `AgentStartContract` before it opens
a provider stream or child process. The exact Rust names may change during implementation, but the
contract has these independent parts:

```text
AgentStartContract
  identity/session/parent
  assignment + context-origin manifest
  model/instructions/capability ceiling/fences
  loop: AgentLoopBinding
  budget: BudgetEnvelope
  reporter: AgentReportChannel
  backend binding

AgentLoopBinding
  logical profile id + revision
  runner kind: native-flux | backend-profile
  immutable source reference + sha256
  entry point: work | review | repair | research | decision | custom
  required operations and runtime features
```

The durable receipt stores the bounded metadata and digests, never the loop source, prompt, tool
catalogue or report bodies. Resume, message, rework and recovery reconstruct the recorded binding.
Changing the project file, role or Fleet template affects later starts only; changing a running
worker requires an explicit new admission/session transition.

### Resolution and inheritance

- A top-level general agent may omit a selector. The host resolves that omission to
  `builtin:adaptive@1` before start and records it as an explicit binding.
- A role or task start resolves its own role/profile binding. It does not copy the parent's loop,
  conversation, budget counters or ambient context. A parent may request an allowed profile by id;
  the host resolves and validates it.
- Fleet writer, reviewer, repair and decision starts have no implicit adaptive fallback. Their
  template or Fleet policy must name a profile compatible with the task kind.
- Capability and budget inheritance are narrow-only ceilings, not context inheritance. A child may
  receive a smaller allowed operation set and a reserved slice of the parent's remaining budget.
- Unknown loop ids, changed hashes, missing operations and unsupported backend features refuse
  before the first model call.

A start-path census test covers CLI/SDK agents, roles through `task`, nested children, Fleet writers,
Fleet reviewers/decision agents, app agents and served/A2A task starts. Adding another constructor
without resolving a loop binding breaks that test.

## Task kinds and Fleet loop policy

Fleet selects behavior from explicit dispatch metadata, not by asking a model to infer it from a
title. The first stable kinds are `implementation`, `documentation`, `research`, `maintenance`,
`review`, `repair` and `decision`; an extension string remains possible. A Board backend may map an
issue type or labelled field into this common dispatch value. If it does not, the agent template's
declared default applies. BoardRef identity and Board profile/backend remain unchanged.

This keeps future Jira, Trello and other Board backends orthogonal. They provide items and mapped
metadata; they are not agent-loop runners and Board does not become a datasource.

Illustrative configuration shape, not yet a frozen TOML schema:

```toml
[fleet.loop_profiles.workhorse]
source = ".flux/loops/fleet-workhorse.flux"
entry = "work"

[fleet.loop_profiles.reviewer]
source = ".flux/loops/fleet-review.flux"
entry = "review"

[fleet.loop_policy]
implementation = "workhorse"
documentation = "workhorse"
review = "reviewer"
repair = "workhorse"
```

Admission snapshots the selected profile, source digest and entry point beside the existing model,
mode, capability, worktree and fence snapshot.

## The Fleet workhorse and review pipeline

The shipped workhorse loop is intentionally small:

```text
read assignment contract
  -> inspect repository instructions + named story/design
  -> establish failing/validation evidence
  -> implement only the assigned contract
  -> run targeted validation
  -> report handoff-ready
  -> return typed FleetHandoff
```

It does not run open-ended intent detection, select another story, inspect unrelated repositories,
coordinate the Fleet or review its own work. A research loop can be exploratory because that is its
declared job; an implementation loop is not.

Review is loop-directed but remains independent:

```text
workhorse handoff
  -> fresh read-only reviewer start at exact commit
  -> reviewer loop invokes the shipped strict-review flow
  -> typed PASS | REWORK(findings) | PARK(findings)
  -> REWORK continues the writer's exact session at entry `repair`
  -> host parks after the existing two-rework ceiling
```

The workhorse reports `handoff_ready` before review begins. The reviewer inherits neither writer
conversation nor writer capabilities, and the writer never applies its own review result. The host
still verifies commits, write sets, commands and evidence; putting behavior in a loop does not move
enforcement into prompts.

## Upstream progress and cooperative yield

`SpawnActivity` remains useful telemetry: the host can say that a child is planning, calling a tool
or finished. It is synchronous, live-only and host-derived. A worker cannot use it to say “the red
test is established”, ask for a decision, or checkpoint a long task.

An `AgentReportChannel` adds a durable acknowledged record:

```text
AgentReport
  report id + monotonic sequence
  agent/session/parent/assignment/loop binding
  phase + state: active | waiting | handoff_ready | budget_warning
  bounded redacted summary
  optional completed/total units
  evidence references, never embedded command output or diffs
  optional attention/decision request
  current budget projection
```

The loop emits reports through a typed stage/operation. The host authenticates the active agent,
checks assignment and sequence, redacts and budgets the payload, persists it, then acknowledges the
event id. A report cannot set Board status, mark a story done, claim a passing test or change Fleet
membership. Fleet and parent status are projections derived from admitted state plus verified
evidence.

An in-process `task` parent receives the same reports through its correlated child channel while it
awaits the final result. A TaskAgentBackend maps them to its typed event stream. TUI, CLI and Fleet
status consume one projection rather than parsing stdout or model prose.

`yield` is a distinct cooperative terminal for the current turn. At a declared safe loop checkpoint
the worker emits a final report plus a durable cursor and returns `yielded`, retaining its assignment,
session, loop binding, capability ceiling and settled budget. The parent or Fleet may resume that
exact cursor after steering or a budget revision. Yield does not pretend to stop an in-flight effect,
does not cancel the agent and does not create another writer. Operator pause (A-140/A-141) remains a
separate control with separate in-flight semantics.

## Hierarchical budgets

Budget means a declared target; limit means a hard ceiling. Both use a single envelope and usage
ledger, fed by [resource-accounting.md](resource-accounting.md)'s immutable physical-usage receipts,
but their scopes compose:

```text
fleet
  -> wave
    -> assignment/task
      -> agent/session
        -> turn
          -> loop phase / model segment / tool dispatch
```

Starting concurrent work reserves a slice from every ancestor. Actual usage settles against the
reservation; unused capacity returns to the parent. A child receives the intersection of its own
profile limits, the caller's delegated slice and every ancestor's remaining hard limit. No config,
resume or backend can widen an admitted envelope silently. Durable reservation ids and idempotent
settlement prevent retry or restart from charging twice or oversubscribing a Fleet.

Dimensions are grouped by what Flux can enforce honestly:

| Dimension | First contract |
|---|---|
| Wall time/deadline | Host clock; stop at a documented safe boundary |
| Model calls | Refuse the next call after the ceiling |
| Input/output/total tokens | Provider usage plus conservative reservation for the next call |
| Tool dispatches and loop iterations | Host-counted |
| Live agents and concurrent tool calls | Existing tree census and per-agent resource ceiling |
| Review/rework attempts | Existing Fleet state-machine ceiling |
| Report/output/evidence/retained bytes | Host-counted bounded payloads |
| Currency spend | C-130 plugs priced/reported cost into the same ledger; unknown price is never zero |
| CPU, RSS, process output, filesystem/disk, network requests | Advertised only when a backend or OS boundary can measure/enforce them; otherwise explicitly unsupported or observation-only |

Approaching a target emits one bounded `budget_warning` report. Hitting a hard limit returns a typed
`budget_exhausted` result naming scope, dimension, spent, reserved and limit. At a safe loop boundary
the agent checkpoints and yields resumably; an effect already in flight follows the same honest
boundary rules as run control. Raising a limit is an explicit revisioned authority action, not a
message in prose.

Usage attribution remains exact: child usage is charged once to the child and rolled up once into
its ancestors. Fleet totals are a projection over the ledger, not a sum of already-rolled-up session
totals. Unpriced calls reserve conservatively under C-130 policy.

## Fleet membership and live capacity

A Fleet worker is one admitted agent/session with one Fleet identity and, for implementation work,
one Board assignment. `max_workers` is the configured hard admission ceiling. C-583 adds a distinct
revisioned `desired_workers` capacity inside that ceiling so main or an operator can scale the live
Fleet without editing TOML or restarting the supervisor.

Scaling up does not create idle nameless agents. It permits the coordinator to admit queued,
dependency-satisfied assignments until desired capacity is reached; explicit `spawn` and `run`
continue to own identity and assignment selection. Scaling down defaults to drain: no replacement is
admitted and active workers retain their identity, assignment, worktree and budget until a safe
yield or terminal result. Forced interruption remains the existing explicit targeted `cancel`
operation rather than an implicit side effect of changing a number.

`fleet.scale` and `flux fleet scale` are two projections over the same typed service and revision.
Status distinguishes configured maximum, desired capacity, admitted/live/draining workers, queued
assignments and budget-limited capacity. The main coordinator may receive the operation; an ordinary
worker cannot change Fleet membership or capacity.

This is separate from nested `task`. A Fleet worker may be granted a bounded task capability, but a
task child is not a Fleet worker, receives no Fleet membership or independent writer worktree and
cannot widen its parent's capabilities, roots, loop or budget. Any per-worker nested-task limit is a
snapshotted agent-start/resource setting (normally zero for a story writer), not “workers per
worker” and not a Fleet scaling actuator.

## Metrics-driven Fleet policy

C-571's ledger also gives the coordinator the feedback signal for a bounded controller. The live
projection groups cost, model calls, tokens, wall/available CPU time, reservations, queue/throughput,
failures, review/rework and gate outcomes by Fleet, wave, assignment, worker and loop phase. Every
metric carries source, freshness and `reported | estimated | observed | unsupported`; absence never
becomes a reassuring zero.

The controller in C-573 operates inside a revisioned operator policy:

- objective and constraints: cost cap, deadline, task priority and verified quality floors;
- allowlisted model/provider ladder and minimum model/effort per task kind or risk;
- minimum/maximum concurrency, per-task budget floors and maximum change rate; and
- freshness threshold, minimum sample, hysteresis and cooldown.

It may change placement, the model/effort selected for a future admission, concurrency within the
admitted capacity and reservation sizes. It cannot increase a hard budget, authorize another model
or provider, widen capabilities, skip review/gates or mark work successful. An active worker changes
only at a C-570 safe report/yield boundary and only if its loop/backend contract admits that actuator;
otherwise the change starts a new explicitly admitted session.

Every adjustment is a durable decision with input/policy digests, old/new values, reason and expected
trade-off. Verified handoff/tests, fresh review, rework and final gates are the quality feedback;
worker confidence and prose are not. The first controller is deterministic and replayable. A model
may later propose a decision, but the host policy remains the authority and cannot grant itself more
budget.

## Backend conformance

`TaskAgentBackend` remains the lifecycle port and gains declared support for:

- arbitrary native Flux loops versus named backend profiles;
- exact resume of a loop binding and entry point;
- durable progress acknowledgements and cooperative yield;
- the budget dimensions it can meter or enforce; and
- cancellation and terminal receipts.

Native Flux is the reference implementation and supports authored Flux loops. A Codex, Claude,
Hermes or Pi adapter may initially support only named profiles that it can map to a documented
harness mode. It must reject an arbitrary `.flux` loop or unsupported checkpoint semantics; “started
with the tool's normal defaults” is not conformance.

Fleet admission remains separate. Backend discovery says what execution can do; the main
coordinator still grants BoardRef, mode, capabilities, fences, budget and lease.

## Delivery order

1. **C-569** — resolve and snapshot `AgentLoopBinding` on every start path.
2. **C-567** — bind Fleet task kinds to native workhorse loop profiles and prove five writers can
   implement instead of recursively explore.
3. **C-570** — add durable typed progress and cooperative yield.
4. **C-572** — run fresh review and same-session repair through explicit loops.
5. **C-575 + C-542 + C-571** — record physical usage, unify local time/token projections, then add
   Fleet reservation/settlement.
6. **C-583** — expose revisioned desired Fleet capacity plus safe drain through one operation/CLI
   contract, keeping nested task children distinct from Fleet workers.
7. **C-573** — tune allowed model/effort/concurrency/reservations from freshness-labelled metrics.
8. **C-552/C-553** — extend the generic backend and foreign CLI adapters against the settled loop,
   report and budget contracts.
9. **C-130** — enforce monetary and rolling per-principal caps through the shared ledger.

C-543/C-544 can follow C-569 without waiting for Fleet budgets. C-556/C-557 consume
C-570/C-571/C-573's typed projections instead of inventing another progress calculation.

## Non-goals

- No loop selects or mutates Board backend state by itself.
- No worker may mark its own story done, apply, push, release or deploy.
- No automatic inference of task kind from issue prose.
- No claim that a foreign harness runs arbitrary Flux-Lang when its adapter cannot do so.
- No replacement of capability ceilings, worktree fences, host evidence or publication gates with
  loop instructions.
- No process-wide memory promise from an in-process library. Hard CPU/RSS/disk limits require a
  backend that actually owns such an isolation boundary.
