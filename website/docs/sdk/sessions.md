---
title: Sessions & persistence
description: "Durable, resumable conversations for an embedded agent: storage injection, the Session handle, and cross-restart resume."
---

# Sessions & persistence

A [`Client`](./overview.md) runs turns against a **session** — one conversation, recorded in an
event store. By default that store is in-memory and dies with the process. Point the client at a
directory instead and its sessions become durable: you can close the process, reopen it, and pick
the conversation back up by id — including a conversation that paused waiting for a human answer.

## Choosing where sessions live

`ClientBuilder::storage` takes a `Storage`:

```rust
use flux_sdk::{Client, Storage};

// Ephemeral (the default) — sessions die with the process.
let client = Client::builder().model("anthropic/opus").build(provider, ".")?;

// Durable — <dir>/events.db + <dir>/flow.db, the same layout the CLI uses, so the directory is
// also readable by `flux sessions`, `flux replay`, and `flux fork`.
let client = Client::builder()
    .model("anthropic/opus")
    .storage(Storage::dir("./agent-state"))
    .build(provider, ".")?;
```

`Storage::custom(events, flow)` is the escape hatch for anything the two conveniences don't cover —
for example a Postgres-backed event store (`EventStore::open_postgres`).

## The Session handle

A `Session` is a cheap, cloneable handle to one conversation on the client's engine. The client
creates a default session at build time (so `Client::run` works out of the box), and hands out more
on demand:

```rust
// The resume seam: open a session persisted by an earlier process.
let session = client.open_session(&saved_id)?;
let out = session.send("What did we decide yesterday?").await?;

// Or start a fresh conversation.
let other = client.create_session()?;

// Read the conversation back (projected from the event store; survives restarts).
for message in session.history()? {
    // …
}
```

`create_session` and `open_session` return a `Session` (`open_session` errors if the id is unknown
to the client's storage); `latest_session` returns `Option<Session>` — `None` when no prior session
exists. `Client::run` and `Client::session_id` remain the one-line path over the default session.

## Resuming across a restart

Because the store outlives the process, a session id persisted on disk resumes cleanly in a new
run. If the earlier turn parked the conversation on a human-in-the-loop `await` (see
[durable flows](./flow-client.md)), the resuming `send` answers the `await` and the flow continues —
the pause survives the restart, not just the process.

```rust
// First process:
let id = {
    let client = Client::builder().storage(Storage::dir("./state")).build(p1, ".")?;
    client.run("Book the earliest flight you can find.").await?;
    client.session_id()?
};

// Later process — same directory, same conversation:
let client = Client::builder().storage(Storage::dir("./state")).build(p2, ".")?;
let session = client.open_session(&id)?;
let out = session.send("Yes, book it.").await?;
```

One engine runs one turn at a time, so concurrent `send`s — on one session or across sessions of
the same client — serialize rather than interleave. Multi-tenant embedders build one client per
agent.

See [`examples/session_resume.rs`](https://github.com/codewandler/flux/tree/main/crates/flux-sdk/examples)
for a runnable, no-API-key version.

## Reading a session back

Every read-back method is a projection over the event store: cheap, side-effect-free, and valid
across restarts. `flux_sdk::observe` re-exports exactly the types they return, so a consumer never
needs a direct dependency on flux's internal event or evidence crates:

| Method | Returns | What it is |
|---|---|---|
| `history()` | `Vec<Message>` | The conversation as the model saw it. |
| `turns()` | `Vec<TurnSummary>` | One row per turn, with its recorded usage. |
| `run_trace()` | `Vec<RunEvent>` | The executed plan as statement/dispatch events — the audit trail. |
| `cost(&pricing)` | `Vec<ModelCost>` | Priced spend per model, against a `PricingTable`. |
| `efficiency()` | `Option<EfficiencySummary>` | Tokens-per-outcome roll-up; `None` before there is anything to summarize. |

```rust
use flux_sdk::observe::{Message, ModelCost, RunEvent, TurnSummary};
use flux_sdk::PricingTable;

let turns: Vec<TurnSummary> = session.turns()?;
let trace: Vec<RunEvent> = session.run_trace()?;
let spend: Vec<ModelCost> = session.cost(&PricingTable::builtin())?;
let history: Vec<Message> = session.history()?;
```

`observe` also re-exports the two stores `Storage::custom` accepts (`EventStore`, `FlowStore`) and
the evidence-gating types `ClientBuilder::groups` / `ClientBuilder::ambient_signals` take
(`ToolGroup`, `SignalMatch`, `Observation`, `KIND_SIGNAL`) — a group hides its `tools` until its
signal fires, and surfacing is sticky-monotonic within a session.

## Replay — re-run a recorded session hermetically

`Session::replay` re-executes a recorded session's plans with **every** leaf-op output served from
the recorded cassette: zero live dispatches, no model call, and side effects never re-fire.

```rust
use flux_sdk::{AgentSink, ReplayReport};

let mut sink = MySink::default();                  // any `impl AgentSink`
let report: ReplayReport = session.replay(None, &mut sink).await?;  // `Some(n)` replays one 0-based turn
assert!(report.diverged.is_none(), "faithful replay");
println!("{}/{} cassette cells consumed", report.cells_consumed, report.cells_total);
```

`ReplayReport` carries `session`, the replayed `plans`, `diverged` (`None` on a faithful replay),
`cells_total` / `cells_consumed`, and `missing_sources` — executions whose plan text is missing from
the log, reported rather than silently skipped. Replay requires a cassette-recorded session (the
default; disabled by `FLUX_CASSETTE=0`); a chat-only session has no cells and errors honestly.
Because nothing dispatches, replay is safe against a client built with a deny-all approver and a
provider that is never called.

## Fork — branch at a decision point

`Session::fork(at)` mints a fresh session correlated to this one, copies its conversation, and
hermetically replays statements `0..at` of the recorded final plan into it. **The original is left
untouched.** Diverge the returned `Fork`, then diff it back against the session it came from:

```rust
use flux_sdk::Fork;

let fork: Fork = session.fork(2).await?;           // branch at top-level statement 2 (0-based)

// Mode A — inject a different value at the fork point's bound statement, skipping the op that
// produced it, then run the plan's tail live through the real envelope.
fork.inject(&serde_json::json!({"status": 503}), &mut sink).await?;

// Mode B — supply an edited plan for the tail instead.
// fork.edit(&edited_ast, &mut sink).await?;

let diff = fork.diff(&session)?;                   // aligned, per-statement
if !diff.identical {
    for row in &diff.rows { /* DiffRow::Same | Plan | Output */ }
}
```

| Method | Signature | Notes |
|---|---|---|
| `id` | `&str` | The fork session's own id. |
| `session` | `Session` | The fork as an ordinary `Session` — read it, replay it, or fork it again. |
| `inject` | `async (&Value, &mut dyn AgentSink) -> Result<()>` | Errors if the fork point is not a `bind` statement, or the diverged tail halts. |
| `edit` | `async (&DraftAst, &mut dyn AgentSink) -> Result<()>` | Errors if the diverged tail halts. |
| `diff` | `(&Session) -> Result<RunDiff>` | `RunDiff { rows, identical }`; a `DiffRow` is `Same`, `Plan` (the statement content differs), or `Output` (same statement, different world). |

A fork's tail runs **live** through the full authorization → approval → guarded-IO envelope, so it
is a real branch, not a simulation — budget for it accordingly. Forking requires a cassette-recorded
session, exactly as `replay` does. The CLI exposes the same three modes as `flux fork`; see
[Time Machine](../agent/time-machine.md) for the concept and the shell-side workflow.

For the *pinned* counterfactual instead — re-running a recorded session under exactly one changed
variable with the rest of the world byte-frozen, and no live dispatch at all — use
`Session::what_if()` / `Client::what_if_over()`, documented in the
[Deterministic Agent Lab](./agent-lab.md).

## Related docs

- [SDK overview](./overview.md) — the front doors and provider setup.
- [Streaming](./streaming.md) — watch a turn unfold live, or cancel it mid-flight.
- [FlowClient](./flow-client.md) — the one-shot flow lifecycle and durable `await`.
- [Deterministic Agent Lab](./agent-lab.md) — golden fixtures, counterfactual what-ifs, and crash resurrection.
- [Time Machine](../agent/time-machine.md) — the same replay/fork/diff model from the CLI.
