---
id: A-98
title: Agent-set wake-up — let a turn schedule its own resumption
pillar: Agent
status: backlog
priority:
epic:
design:
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
