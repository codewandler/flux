---
id: C-601
title: "Give turn cancellation a visible state and sub-agents a wall-clock bound"
pillar: Core
epic: fleet-harness-throughput
status: done
areas: [flux-tui, flux-orchestrate]
note: "cancel works but cannot interrupt an in-flight model call, and the UI keeps spinning with no cancelling state"
---

# Give turn cancellation a visible state and sub-agents a wall-clock bound

## Goal

When an operator cancels a turn, make it observable that cancellation is in progress and bounded.
Today the request is honoured, but the surface gives no indication and the wait has no advertised
ceiling — so a working system looks hung.

## Acceptance

- [x] Cancelling a turn puts its running cards into a distinct *cancelling* state, visibly different
      from both *running* and *cancelled*, so the operator can tell the request was received.
      `SpawnActivityEvent::Cancelling` (non-terminal) → `WorkerStatus::Cancelling`, which latches
      until the terminal so a late tool result cannot repaint the row as ordinary work.
- [x] A research/`task` child carries a wall-clock deadline by default, rather than relying on the
      iteration cap alone. `SpawnLimits::new` now fills in `DEFAULT_SPAWN_WALL_CLOCK` (10 minutes,
      the value the SDK client builders already applied, so no surface gets a shorter bound).
- [x] Failing first, a test proves a cancelled turn with a child in flight reaches a terminal state
      within the advertised bound —
      `cancelling_a_child_in_flight_is_announced_and_terminal_within_the_bound`.
- [x] The bound (in-flight provider call + `SPAWN_CLEANUP_GRACE`) is documented where an operator
      will read it — `website/docs/agent/tui.md`, "How long cancelling takes", reached from the
      `Ctrl-C` section an operator is already reading when they ask.

## Progress

- Landed. Failing-first tests, all named for C-601:
  - `codewandler-flux-orchestrate` — `cancelling_a_child_in_flight_is_announced_and_terminal_within_the_bound`
    (the `Cancelling` announcement precedes the terminal, and the whole cancel completes inside the
    in-flight provider call plus `SPAWN_CLEANUP_GRACE`) and
    `default_sub_agent_limits_carry_a_wall_clock_deadline`.
  - `flux-tui` — `a_cancelled_turns_worker_card_shows_cancelling_instead_of_running` (rendered
    screen, not just the projection) and
    `a_cancelling_worker_is_distinct_from_running_and_from_its_terminal`.
- The announcement is emitted from a second handle on the activity sink, because the child future
  holds `&mut sink` for the whole cancel race — that is the only reason the signal can leave while
  the child is still winding down.

## Notes

- **Cancellation is wired correctly.** `SubAgents::spawn` (`crates/flux-orchestrate/src/lib.rs`)
  runs each child under `cancel.child_token()`, and the parent bounded-awaits the same future so the
  child's own cancel path persists a terminal assistant message before ownership ends;
  `SPAWN_CLEANUP_GRACE` (10s, `crates/flux-runtime/src/lib.rs`) backstops a child that never observes
  the token.
- **But it is cooperative, and a model call is not interruptible.** The token is observed between
  steps. A child mid-request to a large-context provider keeps that request open until the provider
  answers; only then does the grace window start. Observed effect: the operator cancels, the card
  keeps spinning, and nothing indicates the cancel was received.
- **No wall-clock deadline by default.** `ResourceLimits` documents its defaults as "30 iterations,
  no wall-clock deadline". So if a token ever fails to reach a child, nothing bounds it but the
  iteration cap and the token budget.
- Verified terminating correctly on 2026-08-06: after a cancelled coordinator turn with a research
  child, the TUI process measured **1 CPU tick over 3 seconds** and the coordinator store's
  `events.db` had no writes for 3 minutes. The complaint was latency and silence, not a leak.
- Related: [C-599](C-599-fleet-work-is-unobservable-while-it-runs.md) — the same surface gap makes a
  *running* agent invisible; this one makes a *stopping* agent invisible.
