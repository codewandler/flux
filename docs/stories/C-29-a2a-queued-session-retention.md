---
id: C-29
title: "Keep an a2a session minted before the turn gate from being pruned while it waits"
pillar: Core
status: done
priority: 10
epic: review-hardening
design: docs/designs/review-hardening.md
note: "send/subscribe mint the a2a session before acquiring the single-turn gate and never touch updated_at while queued, so a concurrent request's lazy TTL sweep can delete a queued session once its wait exceeds the TTL — orphaning its event rows (they escape the sweep forever) and dropping its spend from usage rollups. The request itself still succeeds (the claimed -32603 is refuted)"
---

# Keep an a2a session minted before the turn gate from being pruned while it waits

## Goal
Stop a mid-flight prune from orphaning a queued A2A session's data. `send`/`subscribe` mint the session
(`create_a2a_session`, which runs the lazy TTL sweep) *before* acquiring the single-turn `turn_gate`
(`crates/flux-server/src/a2a.rs:233`/`:239`, `:270`/`:300`), and `updated_at` is frozen at mint until the
first `record_message` inside `run_turn` — which only runs after the gate. So with a low
`a2a_session_ttl_secs` and a multi-minute turn ahead in the FIFO gate, a concurrent request's mint-time
sweep (`store.rs:436`, `WHERE agent_id='a2a' AND updated_at < cutoff`) deletes the queued session. The
request still *succeeds* (no FK from `events`→`streams`, `read_context` defaults — so the claimed
JSON-RPC -32603 does **not** occur), but the session's event rows become orphans that escape the TTL
sweep forever (unbounded growth — the exact thing the TTL prevents) and its turn drops out of
`cost_summary_all` / usage rollups and `list`/`info`, all of which enumerate `streams`.

## Acceptance
- [x] Failing-first test: with a small `a2a_session_ttl_secs`, hold the `turn_gate` (simulate a long turn),
      mint/queue session X, advance the store clock past the TTL, fire request Y whose mint runs the sweep,
      then release the gate and let X run. Assert `info(s_X)` still succeeds **and** `context.agent_id ==
      "a2a"` (equivalently: X still appears in `aggregate_streams`/`list`, no orphaned events). **Do not**
      assert the request fails — it does not.
- [x] Fix (preserving serialize-turns): mint the session inside the gate, or refresh `updated_at` /
      re-assert existence immediately after acquiring the gate, so a queued session can't age past the TTL.
- [x] The single-turn `turn_gate` serialization guarantee is untouched.

## Progress
- 2026-07-03 filed — 0.2.11 diff review; grounded 🟡 **retention/accounting, re-characterized** (Opus). The
  raw review claimed a liveness failure (-32603); grounding refuted that (the request completes) and found
  the real defect is orphaned, un-prunable event rows + lost spend accounting, only when the TTL is set
  below the longest turn with concurrent traffic (default TTL 1h makes it extreme).
- 2026-07-03 fixed: `send`/`subscribe` (`crates/flux-server/src/a2a.rs`) now acquire the single-turn
  `turn_gate` **before** calling `create_a2a_session` instead of after, so a session's mint and its first
  turn always happen back-to-back under one gate hold — there is no more window where a minted-but-queued
  session sits idle (frozen `updated_at`) for a concurrent request's mint-time TTL sweep to prune. For
  `subscribe` this moved the mint (and the derived `task_id`/`context_id`/initial "working" frame) inside
  the spawned task's gate-acquisition; a mint failure there is now reported as a JSON-RPC error frame
  inside the already-established SSE stream rather than a pre-SSE HTTP error (previously unreachable via
  any test — DB-error-only path). New failing-first test
  `a2a::tests::queued_session_survives_concurrent_sweep_while_gate_held` in
  `crates/flux-server/src/a2a.rs` reproduces the race against the real `send` handler + real
  `create_a2a_session` mint path (real wall-clock 1s TTL crossed by a real `tokio::time::sleep`, not a
  fabricated cutoff): confirmed it fails for the right reason (`session s_1 not found`) against the old
  mint-before-gate ordering, and passes after the fix. No `flux-events` changes were needed — the existing
  `prune_inactive`/`create_session_with_context` seams were sufficient. Gate:
  `cargo test -p flux-server` (9 passed), `cargo clippy -p flux-server --all-targets -- -D warnings`
  (clean), `cargo fmt -p flux-server` (clean).

## Notes
- Evidence: `crates/flux-server/src/a2a.rs:62-106,233,239,270,300`;
  `crates/flux-events/src/store.rs:201-224` (no FK), `:249-276` (create), `:431-455` (prune), `:463-517`
  (append advances updated_at), `:75-95` (read_context defaults), `:839-862` (aggregate_streams);
  `crates/flux-server/src/lib.rs:38,201-218` (TurnGate, default TTL).
- Residual of [C-18](C-18-a2a-session-ttl.md). Design: [review-hardening](../designs/review-hardening.md).
