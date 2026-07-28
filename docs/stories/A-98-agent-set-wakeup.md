---
id: A-98
title: Agent-set wake-up — let a turn schedule its own resumption
pillar: Agent
status: done
epic:
design: docs/designs/agent-set-wakeup.md
note: "the schedule adapter (flux-channels/src/adapters/schedule.rs) is cron-driven and AUTHOR-declared — nothing lets the agent say 'check this again in 20 minutes'; the substrate exists (await/suspension + durable journeys + the event log), so this is a new op over shipped machinery, not new machinery"
---

# Agent-set wake-up — let a turn schedule its own resumption

## Goal
Let an agent end a turn with a *scheduled continuation*: "the deploy is running; wake me in ten
minutes with this context and check it." Today scheduling is authored — `flux app run` drives
declared cron channels (`crates/flux-channels/src/adapters/schedule.rs`) — so an agent that needs to
wait either blocks, or ends the turn and loses the thread. flux already has the durable half
(`await`/suspension, journeys, the event log); what is missing is the agent-facing verb and the
policy question that comes with it.

## Acceptance
- [ ] An op lets the current turn register a future wake-up carrying a prompt plus the context it
      needs, persisted durably — failing-first test asserting the registration survives process exit
      and rehydrates with its context intact.
- [ ] A wake-up fires through the **existing** suspension/resume path rather than a second
      execution route; the resumed turn carries correct telemetry and correlation (the C-26 lesson).
- [ ] Wake-ups are policy-gated and bounded: registering one requires authority (an agent must not
      be able to grant itself unbounded future execution), there is a per-session cap and a maximum
      horizon, and both are configurable and tested.
- [ ] Cancellation exists and is discoverable — pending wake-ups are listable and cancellable from
      the CLI, and are cleared when their session is deleted.
- [ ] The behavior when nothing is running to service the wake-up is defined and documented (fires
      on next start, versus requires a live `flux app run` host) — an honest, tested answer, not an
      accident of implementation.
- [ ] Cost is attributed: a woken turn's spend lands in `flux usage` under the originating session.

## Progress
- (not started — filed from the 2026-07-28 Amp feature-mining pass)
- 2026-07-28: Implemented end-to-end. Design doc: `docs/designs/agent-set-wakeup.md` (op contract,
  durability model, policy story, "who services it", alternatives rejected).
  - **Durability**: three new closed `EventKind` facts in `flux-events` (`WakeupScheduled` /
    `WakeupFired` / `WakeupCancelled`), a `pending_wakeups` projection, and
    `EventStore::{schedule_wakeup,cancel_wakeup,mark_wakeup_fired,pending_wakeups,due_wakeups}` —
    rides the session's own stream, no new store. Failing-first test
    `wakeup_registration_survives_reopen_with_context_intact` (real sqlite file close/reopen) proves
    the durability + context-intact acceptance item.
  - **The op**: `schedule_wakeup` (`crates/flux-flow/src/wakeup.rs`, `WakeupTool`) —
    `{prompt, context, in_secs}`. Declares `Effect::LocalSystem`/`AccessKind::LocalSystem` under its
    own name; the DEFAULT authority derivation (no override) resolves to
    `AuthorityRequirement::host_write("schedule_wakeup")`, which the EXISTING `host.write` default
    policy grant already gates behind approval — zero new policy code. Per-session cap and max
    horizon enforced in `execute()` against the durable `pending_wakeups` count (deliberately NOT a
    `LoopHost` per-turn reservation — see design doc). New `[wakeup]` config table
    (`flux-config`): `enabled` (off by default, surfacing gate), `max_horizon_secs`,
    `max_pending_per_session`.
  - **Fires through the existing path**: `flux_flow::wakeup::service_due_wakeups_on_open` calls
    `FlowEngine::run_turn` — the identical entry an ordinary follow-up message uses — for every due
    wake-up, oldest first, before the caller's own new turn. No bespoke second route; telemetry
    (`turn_id`, `TurnEnded`) and cost land under the originating session for free (the C-26 lesson).
    Wired into the plain `flux` CLI's shared `resurrect_on_open` step (one-shot `run` + REPL) —
    TUI/SDK not wired yet, called out explicitly as a follow-up in the design doc.
  - **Who services it**: implemented "fires on next session open" (tested, deterministic, no
    daemon). Did NOT implement a live `flux app run` proactive poller — a materially separate
    integration (flux-app's Engine is Program/journey-shaped, not a bare-session driver) that this
    pass deliberately left out rather than rush; flagged as real follow-on work, not a silent gap.
  - **CLI**: `flux wakeups list <session|last>` / `flux wakeups cancel <session|last> <id>`
    (`crates/flux-cli/src/wakeup_cmd.rs`), reading/writing the same `EventStore` methods — no live
    engine needed, mirrors `flux sessions`/`flux replay`.
  - **Cleared on session delete**: no new code — every whole-stream delete primitive
    (`prune_older_than`/`prune_inactive`/etc.) already deletes the wake-up along with the rest of
    the stream. Pinned by test `deleting_a_session_clears_its_pending_wakeups`.
  - Verified end-to-end manually too: `FLUX_MOCK_TOOL=schedule_wakeup` through `flux run --yes` →
    `flux wakeups list` shows it with relative time + context marker → letting it go due → the next
    `flux run -c` fires it as its own turn (loud "wakeup · … · firing" line) before the new input,
    then `flux wakeups list` is empty again.
  - Deliberate deviation from the orchestrator's grounding: no new `flux-runtime` `LoopHost` method
    and no `flux-policy` default-grant change — both turned out unnecessary once `host_write`'s
    existing default derivation/grant covered the authority requirement and the cap turned out to
    be a durable (not per-turn) read. Documented with rationale in the design doc.
  - Gate (crate-scoped): `codewandler-flux-events` 74 tests, `codewandler-flux-flow` 223 tests
    (9 new `wakeup::tests::*`), `codewandler-flux-config` 41 tests (1 new), `flux-cli` full suite
    (218 + 5 + 2 + 5 + 1 + 3 + 4 + 13, including the updated `website_contract` CLI-reference
    check) — all green; `clippy -D warnings` and `fmt --check` clean on every touched crate;
    `flux-codegate` layering lint green (13/13, no new violation). Scoped `cargo check` on
    downstream consumers of the new `EventKind` variants (flux-orchestrate, flux-sdk, flux-app,
    flux-channels, flux-tui) also clean. Did not run `cargo test --workspace` (crate-scoped gate
    per the coordinating agent's instruction).
  - Acceptance checkboxes left unchecked per instruction pending review.

## Notes
- Source: [../research/amp.md](../research/amp.md) — Amp's agent-set schedules with self-waking,
  prompt and context preserved across wake-ups.
- Evidence the gap is real: `crates/flux-channels/src/adapters/schedule.rs` — the schedule adapter
  is a declared channel trigger, driven by `flux app run`; there is no agent-initiated path.
- The hard part is not the timer, it is the authority: an agent that can schedule itself can
  consume budget while unattended. Sequence this **after** or alongside **C-130** (monetary budgets
  and quotas) so an unattended wake-up runs under a hard spend cap, and treat that as a real
  dependency rather than a nice-to-have.
- Deliberately narrower than a general job scheduler — this is turn continuation, not cron.
