---
id: A-132
title: "A run-routed `Bus::emit` silently drops its event when the supervisor queue is full — decide the semantics and make them observable"
pillar: Agent
status: backlog
epic: fleet-coordinator
design: docs/designs/fleet-coordinator.md
areas: [flux-app]
note: "filed from A-129's implementor report — pre-existing lossy path, but A-129's admission bound makes a full supervisor queue far more reachable, and `emit` returns `0` for both 'dropped' and 'nobody listening'"
---

# A run-routed `Bus::emit` silently drops its event when the supervisor queue is full — decide the semantics and make them observable

## Goal
`Bus::emit`'s run-routed branch reaches the supervisor with a **`try_send`**
(`crates/flux-app/src/bus.rs:185`) and folds a rejected send into its return value:

```rust
let submission = router.admission.submit();
let sent = sender.try_send(DeliveryMessage::Event { event, run }).is_ok();
```

When the queue is full, `sent` is `false`, the event is dropped, and `emit` returns `0` — the same
`0` its own doc comment (`bus.rs:160-161`) documents as the valid fire-and-forget "no one is
listening" case, and the same `0` returned when the command channel has been dropped entirely
(`delivery_router_does_not_keep_a_dropped_command_channel_alive`, `bus.rs:301-317`). A caller cannot
tell the three apart, and nothing counts the loss.

That directly contradicts the contract A-129 just wrote down one module over: `admission.rs:11-17`
states that at the bound a delivery **waits** — "not dropped and not rejected: every delivery that
reaches the supervisor queue eventually runs" — and records why dropping was rejected (it "silently
loses work a webhook already acknowledged"). The escape hatch is the qualifier *reaches the
supervisor queue*: this path loses the event **before** the queue, so A-129's guarantee never
applies to it.

The path predates A-129. What changed is reachability: the supervisor `mpsc` used to be drained
slowly enough that a full queue was near-unreachable, and A-129's bound now deliberately holds
deliveries so the queue fills as designed. A lossy branch that never fired now fires under exactly
the workload the epic exists for — webhook intake beside a running sweep.

The fix is a real design decision, not a tweak. Converting the `try_send` to an `await`ing `send`
inherits A-129's documented deadlock shape (`admission.rs:25-31`) and makes it worse, because the
emitter here is frequently a journey that is *itself* a delivery holding an admission slot: it would
block on a queue only its own completion can drain. So the decision has to be made explicitly and
justified, and whatever is chosen must stop being silent.

## Acceptance
- [ ] The semantics of a run-routed `emit` that meets a full supervisor queue are **decided in this
      story and written down** next to A-129's tradeoff in `crates/flux-app/src/admission.rs`, with
      the rejected alternatives named. The three candidates, honestly: **block** (cannot lose work,
      can deadlock a journey that emits while holding its own admission slot); **drop loudly**
      (keeps the current liveness, spends the loss on an operator-visible counter); **reject to the
      caller** (a typed error, which changes `emit`'s signature and every call site).
- [ ] Failing-first test: a run-routed emit that is dropped is **distinguishable** from an emit with
      no listener. Both return `0` today (`bus.rs:185-192` vs. `bus.rs:163`), so a test that asserts
      they differ fails on the current tree by construction.
- [ ] Failing-first test: the chosen semantics are **observable** — the natural home is
      `DeliveryLoad` (`admission.rs`), which A-129 already exposes through `App::delivery_load` for
      exactly this "an operator otherwise cannot tell these apart" reason. A drop that only shows up
      as a missing journey is not accepted as a fix.
- [ ] If blocking is chosen: a failing-first test proving the reachable deadlock is *not* reachable
      — a journey that emits a run-routed event while holding an admission slot, with the queue full,
      must still make progress. If it cannot be made safe, that is the argument against blocking and
      belongs in the write-up.
- [ ] `delivery_router_applies_bounded_backpressure` (`bus.rs:320-345`) is updated to encode the
      *chosen* contract rather than the accidental one. It currently pins the drop —
      `assert_eq!(bus.emit("overflow", json!({})), 0)` — and it must keep asserting the A-129
      property it was written for: the rejected event is not left counted as `waiting`.
- [ ] `Bus::emit`'s doc comment stops describing `0` as only the no-listener case.
- [ ] Standard gate green in both workspaces (root + `plugins/`), `cargo fmt --check` included.

## Progress
- (not started)

## Notes
- Filed 2026-07-29 from the fleet-coordinator integration run, out of **A-129's implementor
  report**, which listed this under "Not addressed here (needs its own story)" and was explicit that
  the bound is what promoted it from theoretical to live. A-129 is `done`; this is the piece it
  deliberately left.
- The implementor referred to the pinning test as
  `the_delivery_router_stops_at_the_channel_capacity`. On `main` at filing (base `9721daca`) the
  test that pins this behaviour is `delivery_router_applies_bounded_backpressure`
  (`crates/flux-app/src/bus.rs:320`); same assertion, different name. Do not go looking for the
  other one.
- ⚠ Interacts with A-133 (startup does not strictly precede bus events under `App::run`). Both are
  about which events the run route accepts and when; a fix to either that queues events during a
  window will meet the other's full-queue behaviour.
- Read `admission.rs`'s module docs *before* touching this. The "wait, never drop" choice there was
  argued, not defaulted, and this story either extends that argument to the pre-queue path or
  consciously diverges from it.
