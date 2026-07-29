# Typed session log — session-shape validity by construction

Story: [A-93](../stories/A-93-typed-session-log-epic.md) · Pillar: Agent · Status: design

## The invariant, and why discipline is not enough

flux's session log must always project to a **valid provider history**. Three shapes break every
provider that enforces the Messages contract:

1. an **empty assistant message** (no content blocks, or one empty text block),
2. a **split `tool_use`/`tool_result` pair** — a `tool_use` whose answering `tool_result` was
   dropped, or the reverse,
3. **`user` after `user`** (or `assistant` after `assistant`) — a broken alternation.

This invariant has broken three times, each on a newly added **turn-termination path**: cancel,
compaction, and the iteration cap. Each was fixed pointwise and pinned with a regression test
(`cancellation_keeps_a_valid_user_assistant_session_shape`, `engine.rs:4239`). None of the fixes
made the *next* termination path safe.

The reason is structural, and reading the current code makes it concrete.

### The write seam is untyped and unguarded

The persisted conversation is a projection over exactly two event kinds
(`projection.rs:19`):

```rust
EventKind::Message(m)            => out.push(m.clone()),
EventKind::Compacted { messages } => { out.clear(); out.extend(...) }
```

and both are written through `EventStore` helpers that accept **any** `Message` and append it
unexamined (`store/mod.rs:766`, `:772`):

```rust
pub fn record_message(&self, stream: &str, m: &Message) -> Result<()> {
    self.append(stream, NewEvent::message(m.clone()))?;   // no validation
    Ok(())
}
```

So the type system's current claim about the session log is "it is a sequence of messages" — which
is true of every invalid history too.

### The turn's two writes are far apart, and not paired by any type

A turn appends the **user** message at its start (`engine.rs:420-423`, inside
`begin_turn_lifecycle`) and the **assistant** message at its end (`engine.rs:1177`, inside
`finish_turn`). Nothing links them. Any path that performs the first write and then returns without
reaching the second leaves the log ending on a `user` message — and the *next* turn's opening write
then produces `user` after `user`. The invariant is upheld today only because every known
termination path happens to funnel through `finish_turn_lifecycle` → `finish_turn`.

### A fourth path already exists — and it hand-copies the funnel

`resurrect.rs:425-438` closes a resurrected turn *outside* `finish_turn`, with a comment that says
so out loud:

```rust
// Mirrors `FlowEngine::finish_turn_lifecycle`'s ordering: close the turn on the durable log first
// (`TurnEnded`), then the single assistant message, then tell the sink.
store.event_store().end_turn(...)?;
store.event_store().record_message(session, &Message::assistant_text(answer.clone()))?;
```

That is the predicted fourth termination path, already merged. It happens to be correct — because
someone read `finish_turn` and copied its ordering by hand. That is precisely the discipline this
design replaces with construction. (Note it also writes an `assistant_text(answer)` with no
non-empty check: a resurrect that produced an empty `answer` would write invalid shape #1.)

### Compaction guards one shape, ad hoc, in the wrong place

Compaction snaps its split point backwards so a `tool_result` is never orphaned
(`engine.rs:1449-1455`), using a local helper `has_tool_result` (`engine.rs:1629`). The logic is
right, but it lives in the *caller* — a second compaction call site, or a future history-rewriting
feature (fork, `whatif`, export-and-reimport), gets no protection from it. `flux-sdk`'s fork
(`session.rs:402`) and `whatif` (`whatif.rs:496`) already replay messages through the same raw
`record_message`.

## Design

Make the invalid shapes **unrepresentable at the write seam**, in `flux-events`, so every writer —
current and future, in any crate — is covered by construction rather than by review.

### 1. A typed log handle, not a free function

Replace the untyped pair (`record_message`, `record_compaction`) with a typed handle obtained per
stream. The handle carries the log's **tail state** (what the last appended message was) and exposes
only transitions that preserve the invariant:

```rust
pub struct SessionLog<'a> { store: &'a EventStore, stream: String, tail: Tail, head: i64 }

pub enum Tail { Empty, AwaitingAssistant, Closed }   // the state machine, in one type

impl<'a> SessionLog<'a> {
    pub fn open(store: &'a EventStore, stream: &str) -> Result<Self, LogError>;

    /// Legal only from `Empty` | `Closed`. Moves to `AwaitingAssistant`.
    pub fn open_turn(&mut self, user: Message) -> Result<(), LogError>;

    /// Legal only from `AwaitingAssistant`. Moves to `Closed`.
    pub fn close_turn(&mut self, answer: AssistantMessage) -> Result<(), LogError>;

    /// Replaces the whole projected history. Legal from any state; the *input* is validated.
    pub fn rewrite(&mut self, history: ValidHistory) -> Result<(), LogError>;
}
```

As built (A-100), with three deviations from the sketch above:

- **`LogError`, not a bare `ShapeError`.** The seam does IO, so "this write would have broken the
  shape — nothing was appended" and "the store failed" are separate arms;
  `LogError::shape()` matches the actionable one, and `From<LogError> for flux_core::Error` keeps
  `?` working where a call site only wants to propagate.
- **`open_turn` takes a `Message`**, rejecting a non-`user` role with
  `ShapeError::NotAUserMessage`. The `UserMessage` newtype was never built: A-99 shipped the two
  types the handle actually needs, and a third earns its place only once a second caller wants it.
- **The append is a compare-and-append**, not a plain append after a fresh derivation — see below.

The three invalid shapes map onto this cleanly:

- **user-after-user** — `open_turn` from `AwaitingAssistant` is not a legal transition. It returns
  `Err(ShapeError::TurnAlreadyOpen)` rather than appending.
- **empty assistant** — `AssistantMessage` is a smart constructor: `AssistantMessage::new(blocks)`
  rejects an empty block list and a lone all-whitespace text block. There is no way to build one
  that violates it, so `close_turn` cannot receive one.
- **split tool pair** — `ValidHistory::try_from(Vec<Message>)` walks the sequence and rejects an
  orphaned `tool_use` or `tool_result`, plus role alternation. `rewrite` accepts nothing else, which
  puts compaction's `has_tool_result` snapping logic *inside* the type rather than in one caller.

`Tail` is derived on `open` from the existing `conversation_delta` read (cheap: the kind-filtered
index already serves it) and maintained in memory afterwards. It is a cache of the store's truth,
not a second source of it — `open` re-derives it every time, so a crash or a concurrent writer
cannot leave a stale handle claiming a turn is closed. Concurrency is unchanged: the underlying
`append` keeps its existing `BEGIN IMMEDIATE` semantics (C-25/C-125).

**Re-deriving is necessary but not sufficient** (learned while building A-100). Derive-then-append
is a check-then-act: two handles can both derive `Empty`, both find `open_turn` legal, and both
append — `user`-after-`user` again, now with a type system that looked like it had ruled it out.
So the handle keeps the `stream_seq` its tail was derived from and every transition appends
*conditional on it*, through a new backend primitive `append_if_conversation_head`: the guard read
and the insert run inside the same `BEGIN IMMEDIATE` transaction (Postgres: inside the same
per-stream advisory-locked transaction), and a guard miss appends nothing. The handle then
re-derives and decides again — a miss almost always turns into the honest answer
(`TurnAlreadyOpen`), because the writer that beat us is the one that opened the turn. This is what
the "two handles racing `open_turn` leave exactly one user message" test pins; neutering the guard
makes it fail with two user messages, so the guard is load-bearing rather than defensive.

`EventStore::append_if_conversation_head` is crate-private on purpose. Public, it would be a second
raw way to write the conversation — exactly the bypass this design exists to close.

### 2. The unguarded API goes away — no parallel path

Per the project's clean-cutover rule, `record_message` and `record_compaction` are **removed**, not
deprecated alongside the new handle. Leaving them would recreate exactly the bypass this design
exists to close; a future contributor would reach for the shorter name. This is a breaking change to
a published crate surface ⇒ next release is a MINOR.

Call sites to migrate (the complete set, from `grep record_message`):

| Site | Migration |
|---|---|
| `flux-flow/engine.rs:422` (user, turn start) | `open_turn` |
| `flux-flow/engine.rs:1177` (assistant, `finish_turn`) | `close_turn` |
| `flux-flow/engine.rs` compaction (`record_compaction`) | `rewrite(ValidHistory)` — snapping moves into the type |
| `flux-flow/resurrect.rs:438` | `close_turn` — the hand-mirrored path becomes the enforced one |
| `flux-sdk/session.rs:402` (fork) | `rewrite` |
| `flux-sdk/whatif.rs:496` | `rewrite` |
| `flux-cli/session.rs:314` (fork) | `rewrite` |
| `flux-cli/export_cmd.rs`, `flux-server` tests, `flux-events` tests | test-only; use the typed API |

Note the fork/`whatif` sites currently replay message-by-message; `rewrite` is both the correct
shape guarantee *and* one append instead of N.

### 3. The provider-wire seam is unchanged

The typed log projects to `Vec<Message>` exactly as today (`projection::conversation`), and each
wire codec keeps consuming that. This design deliberately does **not** push the types down into the
codecs: the codecs' job is per-provider shape translation, and they already work. The invariant
being fixed is about what gets *written*, not how it is rendered.

## What the migration forced (A-101)

Two things the sketch above did not anticipate, both found by migrating `flux-flow` onto the handle
and both pre-existing bugs rather than migration artifacts:

- **A turn can have no user message.** `start_flow_turn` (SDK `start_flow`, the app runner, the
  voice driver) begins a turn with `user_input: None`, yet `finish_turn` still persists its answer —
  so a flow-driven session's log *opened on an `assistant` message*, which is not a valid provider
  history. Such a turn now opens with a synthetic `[<flow name>]` user message naming its trigger.
  The alternative considered and rejected was pairing both writes or neither (drop the answer): it
  keeps the log valid too, but a later conversational turn would lose the flow's answer entirely,
  which matters most exactly where flow turns are used (voice, app runners).
- **An abandoned turn must be closeable by someone else.** A turn that died between its two writes
  and was not resurrected leaves the log owing an answer; every later `open_turn` would be
  `TurnAlreadyOpen` forever. `begin_turn_lifecycle` closes it with `(turn interrupted)` before
  opening its own. This is append-only — the crashed turn's telemetry and run trace are untouched —
  and it is what turns the invariant from a trap into a self-healing property.

Both are the same lesson: the state machine is only as good as its handling of the states that
already exist in the wild. The type made them visible; it did not create them.

## What this does not do

- It does not validate *content* (a `tool_result` whose `tool_use_id` matches no in-flight call
  within the same assistant message is caught; semantic correctness of the payload is not).
- It does not make the live pre-release smoke gate redundant — it makes the gate stop being the
  *only* net for this class.
- It does not change compaction's policy (what to summarize, how much to keep), only where the
  boundary-snapping rule lives.

## Verification

- Failing-first: a test that calls `open_turn` twice in a row must fail to compile-or-return-Err
  **before** the typed API exists (today it silently produces `user`-after-`user`).
- A property test over `ValidHistory::try_from` across generated message sequences: accepted iff the
  sequence satisfies all three invariants.
- The existing `cancellation_keeps_a_valid_user_assistant_session_shape` test must stay green
  unmodified — behaviour lock.
- A test proving `resurrect`'s path now goes through `close_turn` (the hand-mirroring is gone).
- Full gate in both workspaces: `cargo test --workspace`, `clippy -D warnings`, `fmt --check`, the
  `flux-codegate` layering lint.

## Stories

| ID | Title |
|---|---|
| A-99 | `ValidHistory` + `AssistantMessage` smart constructors in `flux-events` (the shape rules, unit-tested standalone) |
| A-100 | `SessionLog` typed handle with `Tail` state machine; `open`/`open_turn`/`close_turn`/`rewrite` |
| A-101 | Migrate `flux-flow` (engine turn start/end, compaction, resurrect) onto the handle; delete `record_message`/`record_compaction` |
| A-102 | Migrate `flux-sdk` + `flux-cli` fork/`whatif`/export call sites onto `rewrite` |
