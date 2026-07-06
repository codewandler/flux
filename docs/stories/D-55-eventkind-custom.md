---
id: D-55
title: EventKind::Custom — app-defined facts in the unified event log
pillar: Agent
status: done
epic: consumer-gaps
note: "from the 2026-07-06 downstream-consumer review: consumers with app-level facts (audit trails) can't ride events.db — kind.rs is a deliberately closed enum — so they build parallel stores; this story consciously revises that doctrine by one open variant"
---

# EventKind::Custom

## Goal
Let a consumer append **app-defined facts** to flux's unified append-only event log (`events.db`)
without flux interpreting them — one open variant in the otherwise-closed `EventKind`, so app audit
trails, domain events, etc. share the substrate (ordering, account scoping via `EventContext`,
projections) instead of living in parallel stores.

## Why (evidence)
`EventKind` (`crates/flux-events/src/kind.rs:22`) is a closed enum; its module doc (kind.rs:1-10)
states that position. The reviewed downstream consumer needed an account-scoped, ordered audit log
and — unable to ride the event log — reimplemented one on `flux_capabilities::datasource`, flagging
the intended fix in its own code: "the sanctioned future upgrade … is flux-events (which first
needs an `EventKind::Custom` variant upstream)".

## Design position (doctrine revision, deliberate)
- `Custom { name: String, payload: serde_json::Value }`, adjacently tagged like every variant
  (`{"kind":"custom","data":{...}}`). `name` namespaces the fact (e.g. `"audit.tool_call"`);
  `payload` is opaque — **flux never interprets it**.
- The enum stays **NOT `#[non_exhaustive]`**: the compile-forced exhaustive match is the module's
  stated design value — every flux projection must consciously decide what `Custom` means for it
  (almost always: skip).
- Module doc updated to state the revised position: the set of *flux* facts stays closed; `Custom`
  is the one extension point for *app* facts.

## Acceptance
- [x] `EventKind::Custom { name, payload }` added; serde round-trip test (incl. adjacent-tag shape)
      and an old-log decode test (existing kinds unaffected).
- [x] Every exhaustive `EventKind` match in flux (projections, stores, sinks — grep them all) gets an
      explicit `Custom` arm with a deliberate decision (skip/ignore documented per site); test that
      the conversation/cost projections are unaffected by interleaved Custom rows.
- [x] Appending works through the existing `EventStore::append`/`NewEvent` path with `EventContext`
      scoping (test: append Custom rows under an account, read them back filtered, other projections
      unchanged).
- [x] Module doc revision; full gate green; consumer-compat: `cargo check` in the downstream
      consumer workspace (if it
      matches `EventKind` exhaustively anywhere, that one-arm break is enumerated in the adoption
      story — everything else additive).

## Progress
- 2026-07-06 filed from the consumer review; implementation started same day.
- 2026-07-07 implemented and closed:
  - `crates/flux-events/src/kind.rs`: added `EventKind::Custom { name: String, payload:
    serde_json::Value }` (adjacently tagged, wire shape `{"kind":"custom","data":{"name":...,
    "payload":...}}`), a `kind_tag()` arm (`"custom"`), and NOT `#[non_exhaustive]` per the design
    position. Module doc (kind.rs:1-23) revised: the set of *flux* facts stays closed and
    exhaustively matched; `Custom` is documented as the single extension point for *app* facts, and
    why the enum still isn't `#[non_exhaustive]` (an open registry inside a closed enum would defeat
    the compile-time guarantee the doc exists to state).
  - Grepped every `EventKind::`/`.kind` match site in the workspace (`grep -rn "EventKind::"
    crates/` plus a follow-up `\.kind {` sweep to catch matches not spelled `EventKind::`). Exactly
    one match was truly exhaustive (no wildcard) and needed a compile-forced arm: `kind_tag()` in
    `kind.rs` itself. Every other match site already uses a wildcard `_ =>` / `if let` — each was
    read and confirmed its fallback behavior (skip/ignore) is already correct for `Custom` (none is
    a loud "unknown kind" catch-all); see Notes for the full list.
  - Tests added (`cargo test -p flux-events`, 47 passed, 0 failed):
    - `kind::tests::custom_round_trips_with_adjacent_tag_shape` — serde round-trip incl. the exact
      adjacent-tag JSON shape, plus a non-object (scalar) payload.
    - `kind::tests::pre_d55_logs_without_custom_still_decode` — raw pre-D-55 JSON rows (
      `session_started`, `turn_started`, `call_usage`, `turn_ended`) still decode after adding
      `Custom`.
    - `projection::tests::custom_events_interleaved_dont_affect_conversation_cost_or_turns_projections`
      — a stream with `TurnStarted`/`Message`/`CallUsage`/`TurnEnded` interleaved with `Custom` rows
      at 4 positions (before the turn, mid-turn twice, after close) folds `conversation`,
      `cost_summary`, and `turns` to identical results vs. the same stream without the `Custom` rows.
    - `store::tests::custom_events_append_and_read_back_scoped_by_account` — appends a `Custom` row
      via `EventStore::append`/`NewEvent::new` under an `EventContext::for_account` session,
      confirms `stored.context` carries the scoping, reads back via `account_streams` +
      `load_stream`, verifies the payload is byte-identical after the SQLite round trip, and that
      `conversation`/`msg_count` are unaffected by the Custom row's presence.
  - Full gate green: `cargo build --workspace`, `cargo test --workspace` (all crates, 0 failed),
    `cargo clippy --workspace --all-targets -- -D warnings` (clean), `cargo fmt --check` (clean,
    after running `cargo fmt`), `(cd plugins && cargo fmt --check)` (clean).
  - Consumer-compat: `cargo check --workspace` in the downstream consumer workspace (path-dependency
    on this repo's `flux-events`) is clean. Its one `EventKind` match uses a wildcard `_ => {}`
    default-deny arm — additive, no break; nothing to enumerate in the adoption story beyond what
    it already names.

## Notes
- Adoption story filed in the consumer's own repo: migrate its parallel audit store onto Custom
  events — its code names this exact path.
- Match sites given conscious `Custom` decisions (grep-complete list; the compile-forced one is
  marked, the rest were verified wildcard-correct rather than edited, since editing a
  non-compile-forced test-only wildcard would be churn without changing behavior):
  - `crates/flux-events/src/kind.rs:145` (`EventKind::kind_tag`) — **compile-forced**, added
    `EventKind::Custom { .. } => "custom"`.
  - `crates/flux-events/src/projection.rs:22` (`conversation`) — wildcard `_ => {}`, verified
    correct (Custom never touches the conversation fold).
  - `crates/flux-events/src/projection.rs:39` (`run_trace`) — wildcard `_ => None`, verified correct.
  - `crates/flux-events/src/projection.rs:97` (`turns`) — wildcard `_ => {}`, verified correct
    (Custom doesn't affect turn telemetry).
  - `crates/flux-events/src/projection.rs:171` (`observations`) — wildcard `_ => None`, verified
    correct.
  - `crates/flux-events/src/projection.rs:240` (`corpus_rows`) — wildcard (no catch-all arm needed;
    only two variants matched, everything else implicitly skipped via the `if`/`continue` shape),
    verified correct.
  - `crates/flux-events/src/projection.rs:564` (`cost_summary`, `if let EventKind::CallUsage`) —
    verified correct (non-CallUsage rows, including Custom, are ignored by construction).
  - `crates/flux-events/src/store.rs:485` (`append`, session `model` column update) — wildcard
    `_ => None`, verified correct (Custom carries no model).
  - `crates/flux-events/src/store.rs:499` (`append`, `msg_count` maintenance) — wildcard `_ => {}`,
    verified correct (Custom doesn't change the conversation length).
  - `crates/flux-events/src/store.rs:1093` (test-only `conversation_delta` fold helper) — wildcard
    `_ => {}`, verified correct.
  - `crates/flux-flow/src/loop_host.rs:1246` (`load_persisted_conversation`) — wildcard `_ => {}`
    with an existing comment noting the delta is already kind-filtered to message/compacted at the
    SQL layer, so Custom rows never reach this fold at all; verified correct.
  - `crates/flux-flow/src/engine.rs:3183` (test-only `filter_map` over `CallUsage`) — wildcard
    `_ => None`, verified correct.
  - `crates/flux-orchestrate/src/lib.rs:1510` (test-only `filter_map` over `CallUsage`) — wildcard
    `_ => None`, verified correct.
  - No sink/CLI/TUI site matches `EventKind` at all (`flux-cli`/`flux-tui`'s `.kind` uses are on
    unrelated types — `Observation.kind: String`, `KeyEvent`/`MouseEvent`, `ChannelDecl.kind`).
