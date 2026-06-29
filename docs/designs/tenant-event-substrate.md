# Design: tenant/agent context envelope on the event log

**Status:** implemented (story [D-02](../stories/D-02-tenant-event-substrate.md)) · **Layer:** L2
(`flux-events`) · **Owner:** Timo

## Why

`flux-events` is flux's one append-only log; the conversation / run-trace / turn-metrics views are
*projections* over it. A downstream **multi-tenant** service (the managed **managed-agents** service: run
persistence **R-04**, the **M4** transparency surface) wants to persist and replay runs as *projections over
this same log* — "flux is already event-sourced … build on that substrate, not a new one." For that the log
must record **whose** run each stream is. Tagging it when the run is written is cheap; tagging it after R-01
already writes untagged runs is a migration. So this lands **now**, additively — the single-tenant CLI path
is byte-for-byte unchanged.

## Shape — context lives on the **stream (run)**, not per event

A *stream* in this store is one session — and, downstream, one **run**. Its owning account, the agent
identity that served it, and a cross-run correlation id are **fixed for the run's whole lifetime**. So the
envelope lives on the `streams` registry (set once at creation), and the `events` table is **not touched**.
Per-event columns were rejected: redundant on every row, and not indexable as a registry column is.

```rust
pub struct EventContext {            // all optional → an empty envelope is the single-tenant case
    pub account: Option<String>,        // the tenant that owns the run
    pub agent_id: Option<String>,       // the agent identity that served it
    pub agent_version: Option<String>,  // which revision ran (recorded in the transcript)
    pub correlation_id: Option<String>, // ties related runs together (e.g. an A2A context_id)
}
```

- **Write:** `EventStore::create_session_with_context(model, &ctx)`. The 1-arg `create_session(model)`
  delegates with `EventContext::default()`, so the 9 existing call sites need **no change**.
- **Read:** `context` is surfaced on `StoredEvent`, `SessionInfo`, and `SessionSummary` — read once per
  load from the registry and stamped on every event of the stream (all events of a stream share it).
  Ad-hoc (non-`s_<n>`) and untagged streams carry an empty envelope → identical behaviour to before.
- **Account-scoped reads:** `list_for_account(account, limit) -> Vec<SessionSummary>` (the runs for one
  tenant) and `account_streams(account) -> Vec<String>` (their stream ids). Backed by a new
  `idx_streams_account`. Transcript replay reuses the **existing** `conversation`/`turns` projections —
  no new projection.
- **Migration:** additive + idempotent — `init` adds the four nullable columns via a `PRAGMA table_info`
  guard (SQLite has no `ADD COLUMN IF NOT EXISTS`), so a fresh store and a pre-existing one converge on one
  schema with no destructive step. The `events` table and every existing projection are unchanged.

## Downstream consumption (the substrate in use)

A multi-tenant service shares one `events.db` and folds it into per-account transcripts as projections —
no parallel store:

```rust
let store = EventStore::open("events.db")?;

// At run start: tag the run with its tenant/agent identity.
let run = store.create_session_with_context(
    model,
    &EventContext { account: Some(account_id), agent_id: Some(agent.id),
                    agent_version: Some(agent.version), correlation_id: Some(context_id) },
)?;
// … the engine appends messages / run / turn events to `run` exactly as in single-tenant mode …

// Transparency / run persistence: replay ONE tenant's runs, isolated from every other account.
for s in store.account_streams(&account_id)? {       // only this account's streams
    let transcript = store.conversation(&s)?;         // existing projection, unchanged
    let telemetry  = store.turns(&s)?;
    // → R-04 run record / M4 transparency view, derived from the log
}
```

## Testing (hermetic)
- **Round-trip:** a context set at creation is read back on every `StoredEvent` of the stream and on
  `info`/`list`.
- **Isolation:** two accounts' runs stay separate under `list_for_account` / `account_streams`; an unknown
  account sees nothing; the global `list` still sees both.
- **Single-tenant unchanged:** `create_session` yields an empty envelope everywhere and an untagged run
  never surfaces in an account-scoped read.
- **Durability:** a file-backed store reopens with the context intact (proves the additive migration is
  idempotent across the process boundary).

## Non-goals / follow-ups
- Wiring real context values at any flux call site — the CLI stays single-tenant; the A2A `context_id`
  (today read by `extract_context_id` and only echoed) is the natural `correlation_id` source, and
  persona/context-**from-file** at `flux app run` time is story [D-11](../stories/D-11-app-runner-ergonomics.md).
- Per-account ACLs / encryption; cross-account aggregate reads; an account registry table.
- Threading context through sub-agent spawn (orchestrate) — a later story if child runs must inherit it.
