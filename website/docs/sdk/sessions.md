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

## Related docs

- [SDK overview](./overview.md) — the front doors and provider setup.
- [Streaming](./streaming.md) — watch a turn unfold live, or cancel it mid-flight.
- [FlowClient](./flow-client.md) — the one-shot flow lifecycle and durable `await`.
