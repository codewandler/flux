---
id: C-83
title: Bound A2A resource use and stop holding the registry mutex across DB I/O
pillar: Core
status: done
priority: 10
epic: harness-hardening
design: docs/designs/harness-hardening.md
note: "DoS (Medium) — unbounded turn queue, id-echo amplification, un-swept push map, std-mutex across block_on"
---

# Bound A2A resource use and stop holding the registry mutex across DB I/O

## Goal
Add the missing resource limits on the authenticated A2A surface (isolation itself is solid — this is
about amplification and runtime starvation): the non-blocking turn queue and session minting are
unbounded; the attacker-controlled JSON-RPC `id` is re-serialized into every SSE frame and buffered in
an unbounded channel; the push-config map is never swept when its session is TTL-pruned; and
`mint_and_register` holds a `std::sync::Mutex` across synchronous `block_on` DB I/O, serializing all
mints process-wide and parking tokio workers.

## Acceptance
- [x] Failing-first test: a per-realm cap rejects excess in-flight non-blocking turns with `-32603`
      instead of accumulating unbounded spawned tasks + sessions.
- [x] Echoed JSON-RPC `id` is capped/normalized; the SSE channel is bounded (back-pressured mpsc).
- [x] Push-config entries are removed on session prune / task finish.
- [x] The registry mutex is not held across DB I/O (move prune/mint off the async workers, e.g. `spawn_blocking`).

## Progress
- 2026-07-15 — implemented all four bounds in `crates/flux-server/src/a2a.rs` (flux-pg touched for a
  doc caution only). Details:
  1. **Per-realm in-flight cap.** New `max_inflight_per_realm()` (default 64, override
     `FLUX_A2A_MAX_INFLIGHT_PER_REALM`) is enforced in `mint_and_register` *before* any DB work, so
     a flood is rejected as `MintError::RealmBusy` → JSON-RPC `-32603` without minting throwaway
     sessions or spawning background turns. Counted over the live map per `realm`, so it is
     per-realm not process-global. Test: `per_realm_in_flight_cap_rejects_excess_turns`.
  2. **Id normalization + bounded SSE channel.** `normalize_rpc_id` (capped at `MAX_RPC_ID_LEN=128`
     chars; bool/array/object → null) runs once at the `dispatch_rpc` chokepoint, so every echo
     path (plain responses + per-frame SSE) inherits the bounded id. The `message/stream` channel
     is now a bounded `mpsc::channel(SSE_CHANNEL_CAPACITY=256)`; the async spawner awaits capacity
     (real back-pressure) and the synchronous `StreamSink::text_delta` cancels the run on a full or
     closed buffer (a consumer that stopped draining). Tests: `normalize_rpc_id_*`,
     `stream_sink_cancels_when_the_consumer_stalls`.
  3. **Push-map sweep.** `TaskRegistry::finish` now drops the task's push config (the terminal
     delivery already snapshotted it), and a mint-time `sweep_orphaned_push_configs` drops configs
     whose session was TTL-pruned — bounding the map to live tasks over the session lifecycle.
     Test: `finish_sweeps_the_push_config`; the A-57 delivery conformance test still passes.
  4. **No std mutex across DB I/O.** `mint_and_register` is now `async`: a dedicated async
     `mint_gate` serializes mints (preserving the C-29 resolve→register atomicity) while the
     `std::sync::Mutex` (`live`) is held only for two brief in-memory phases; the prune + find/mint
     DB round-trip runs on the blocking pool via `spawn_blocking`, so a PG `block_on` never parks a
     tokio worker nor serializes every mint on a std mutex. flux-pg `block_on` gained a caller
     caution documenting the hazard.
- No public-API changes (`TaskRegistry`, `mint_and_register`, `MintError` are `pub(crate)`/private).
  New env knob `FLUX_A2A_MAX_INFLIGHT_PER_REALM` is additive. `cargo test -p flux-server` +
  `cargo clippy -p flux-server --all-targets -D warnings` green; flux-pg clippy/tests green.

## Notes
- `crates/flux-server/src/a2a.rs:838` (queue), `:1724`/`:1559` (id echo), `:1373`/`:132` (push map),
  `:364` (mutex) + `crates/flux-pg/src/lib.rs:193` (`block_on`).
- Design: [harness-hardening](../designs/harness-hardening.md).
