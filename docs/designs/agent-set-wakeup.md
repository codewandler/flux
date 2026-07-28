# Agent-set wake-up — let a turn schedule its own resumption

**Status:** implemented (2026-07-28) · **Story:** [A-98](../stories/A-98-agent-set-wakeup.md)

## Goal

Today scheduling is *authored*: `flux app run` drives declared cron channels
(`crates/flux-channels/src/adapters/schedule.rs`). Nothing lets a live turn say "wake me in ten
minutes and check the deploy." flux already has the durable half — the event log, and the
suspension/resume machinery a turn already uses to park on a decision. This is a new **op** over
that shipped machinery, not new machinery: an agent-facing verb (`schedule_wakeup`) plus the policy
question that comes with letting an agent grant itself future, unattended execution.

## The op contract

`schedule_wakeup(prompt: String, context: Option<String>, in_secs: u64) -> wakeup_id`

- `prompt` becomes the "user" input of the turn the wake-up fires as.
- `context` is background captured *now* and replayed back unchanged when it fires — wrapped in a
  `ContextBlock` (the same containment machinery `consult`/A-21 knowledge injection use) so it is
  never treated as fresh instructions when it resurfaces, however old or attacker-adjacent its
  content turns out to be.
- `in_secs` is relative, not an absolute timestamp — simpler for a model to reason about ("in 10
  minutes") and trivially bounded against the configured maximum horizon.
- Returns the wake-up's id (a store-minted ULID — the same id the store already mints for every
  event when the caller doesn't supply one) so the turn can tell the user what to cancel.

Declared effects: `Effect::LocalSystem` + `AccessKind::LocalSystem`, named `schedule_wakeup` — the
**default** `Tool::authority_requirements` adapter derives `AuthorityRequirement::host_write("schedule_wakeup")`
from that pair alone (see `crates/flux-runtime/src/lib.rs`'s `settings.save` test for the identical
shape). No override, no new `Action` string, no new `FlowEffect` tag.

## Durability model

Registrations ride the **existing event log**, not a new store (event-store-unification canon):
three new closed `EventKind` facts — `WakeupScheduled { fire_at_ms, prompt, context }`,
`WakeupFired { wakeup_id, turn_id }`, `WakeupCancelled { wakeup_id }` — appended to the *session's
own stream*. A wake-up's identity is the `WakeupScheduled` event's own store-minted `id`; there is
no separate id-generation scheme. A pure fold (`projection::pending_wakeups`) replays
scheduled-minus-{fired,cancelled} into the live set, exactly like `turns`/`conversation`.

Two direct, useful consequences of this choice:

- **"Survives process exit"** (the acceptance's failing-first test) is a real close/reopen of the
  same sqlite file, not an in-memory illusion — the wake-up is just more rows in the log.
- **"Cleared when the session is deleted"** needs no new code at all: every existing session-delete
  primitive (`prune_empty`, `prune_inactive`, `prune_older_than`, …) deletes the *whole stream*,
  and a pending wake-up is nothing but events in that stream. Deleting the session deletes it.

`EventKind` is a **closed** enum (not `#[non_exhaustive]`) specifically so every projection is
forced to decide what a new fact means — checked before adding these variants: the only truly
exhaustive match over the full enum in the whole workspace is `EventKind::kind_tag()` inside
`flux-events` itself; every other site (`flux-tui`, `flux-orchestrate`, the sqlite/postgres
backends' side-effect matches) already ends in a wildcard `_ => {}` arm. Adding these three variants
is therefore contained entirely to `flux-events` plus one `kind_tag` arm — no fan-out edit across
the six other crates that also match on `EventKind`.

## The policy story

"An agent must not be able to grant itself unbounded future execution" is enforced two ways,
deliberately kept separate:

1. **Authority to register at all** — `host_write("schedule_wakeup")` resolves through the
   **existing** `default_local_grants()` `host.write` grant, which is already `requires_approval:
   true` for every subject. Registering a wake-up prompts for approval exactly like any other
   host-state mutation (`settings.save`) — no new policy plumbing. An operator who wants it silent
   for a trusted workspace already has the tools to do that (`--yes`, an "always allow" rule);
   nothing new is introduced.
2. **Hard bounds regardless of approval** — a configurable per-session cap and maximum horizon,
   enforced inside `execute()` against the durable projection (see below), so even an approved
   registration can't reach further than the operator configured.

**Deliberately rejected: a new domain-specific `FlowEffect`/semantic-effect tag** (e.g. `"schedule"`
or `"calendar"`). C-184 (shipped the same day as this story) retired `FlowEffect::Calendar` for
exactly this failure mode — a domain *noun* in a vocabulary that is supposed to classify
*consequences* (delete, money, external send, host-state mutation). "Registers a durable host-state
fact that will trigger future autonomous execution" **is** a host-state mutation; it needed no new
noun, and inventing one today would have repeated the mistake the same day it was fixed. It also
would have touched `flux-spec`, which is on the independent plugin-protocol 1.x line — out of
scope and actively worked by a concurrent session.

**Deliberately rejected: a `LoopHost` per-turn reservation** (the `reserve_consult_call` pattern
A-96 established). `LoopHost`'s contract is explicit: turn accounting "resets every turn." A
per-session cap is durable and must survive across many turns, so it does not fit that seam — it is
enforced instead by reading `EventStore::pending_wakeups(session)` once per call (a single indexed
stream read), which is both simpler and correct for the actual shape of the bound.

Config (`[wakeup]`, mirrors `[consult]`):

```toml
[wakeup]
enabled = true              # off by default — the op is not registered at all otherwise
max_horizon_secs = 86400     # default: 24h
max_pending_per_session = 5  # default: 5
```

## Who services it — the honest answer

**Implemented: fires on next session open.** Every turn-entry point through the plain `flux` CLI
(one-shot `flux run`, the REPL, `/resume`) already runs a shared "before the new turn" step
(`resurrect_on_open`, D-183) that finishes a crash-interrupted turn first. This story adds a
symmetric step to the *same* call site: `flux_flow::wakeup::service_due_wakeups_on_open` fires
every wake-up whose `fire_at_ms` has already elapsed, oldest first, **each as its own ordinary
`FlowEngine::run_turn` call** — before the caller's own new input runs.

**Not implemented: a live, always-on `flux app run` proactive poller.** This is the honest
scope cut. A live host *could* fire a due wake-up the instant it becomes due instead of waiting for
the session to be reopened, but wiring that in safely means teaching `flux-app`'s `Engine` (built
around Programs/journeys, not bare sessions) to drive an arbitrary conversational `FlowEngine` on a
timer — a second, materially different integration surface, not a small addition, and one that
risks the concurrent flux-app/flux-channels work landing the same day. "Fires on next open" is a
complete, honest, fully-tested answer on its own (the acceptance explicitly allows either); the
live-host proactive path is real follow-on work, not a silent gap — see Progress/Notes on the story
for the explicit flag.

**Not wired in this pass:** `flux-tui` and `flux-sdk` have their own turn-entry points and do not
yet call `service_due_wakeups_on_open` (only the plain `flux` CLI does). Same reasoning: contained
blast radius for a large story: this pass proves the mechanism end-to-end on one surface with full
tests; wiring the same one-line call into the other surfaces is small, separable, mechanical work.

## Fires through the existing suspension/resume path (not a second execution route)

The C-26 lesson: a resumed continuation that doesn't call `begin_turn`/`end_turn` is invisible to
`turns()`/cost rollups. `service_due_wakeups_on_open` does not reimplement any turn mechanics — it
calls `FlowEngine::run_turn(session_id, &composed_input, sink)`, the exact same public entry every
ordinary follow-up message and every suspension resume already goes through. `run_turn` decides for
itself whether the session is fresh or has a pending suspension; the wake-up firing code doesn't
need to know or care. Telemetry, turn id, and cost attribution all fall out for free because
nothing bypasses the normal path.

## Cost attribution

Because firing a wake-up is exactly `agent.run_turn(session_id, prompt, sink)` on the wake-up's own
originating session, the fired turn's `TurnStarted`/`TurnEnded`/`CallUsage` events land in that same
session's stream like any other turn. `flux usage` (backed by `cost_summary`/`cost_summary_all`,
which fold the whole store) attributes the spend under the originating session with no new code.

## Cancellation / discoverability

`flux wakeups list <session|last>` and `flux wakeups cancel <session|last> <wakeup_id>` — a new
top-level subcommand (`crates/flux-cli/src/wakeup_cmd.rs`), reading/writing the same
`EventStore::pending_wakeups`/`cancel_wakeup` used by the op and the firing path. No live engine is
needed for either (mirrors `flux sessions`/`flux replay`'s `open_event_store()` pattern).

## Non-goals

- Not a general job scheduler / cron — no recurring wake-ups, no external triggers. One wake-up is
  one future turn on the session that registered it.
- Not a daemon. Nothing in this story starts a new always-running process.
