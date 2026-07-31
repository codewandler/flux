---
id: C-372
title: Give every long-lived stream a ceiling and a permit
pillar: Core
status: backlog
epic: serving-surface-and-turn-outcome-residuals
design: docs/designs/serving-surface-and-turn-outcome-residuals.md
note: "the SSE route is deliberately timeout-exempt and the provider client uses an inactivity timeout, so a drip-feeding provider holds a permit and the global turn gate forever; tasks/resubscribe takes no permit at all"
---

# Give every long-lived stream a ceiling and a permit

## Goal

Bound the streaming paths in time, and close the one streaming route that takes no concurrency
permit at all.

## Acceptance

- [ ] An SSE turn has a wall-clock ceiling that does not defeat legitimate long turns — the route
      stays exempt from the ordinary request timeout, but is not unbounded.
- [ ] An SSE request parked on the turn gate has a bound (C-371's bound applies here too).
- [ ] `tasks/resubscribe` (`crates/flux-server/src/a2a.rs:1433-1470`) takes a `WorkPermit` or a
      separate stream-concurrency cap; today only the 120/min rate limit gates it, so a principal
      can accumulate thousands of open streams.
- [ ] `/health` and the agent-card routes carry a rate limit; in multi-agent mode
      `agent_card_multi` (`a2a.rs:601-612`) resolves unauthenticated, so a DB-backed resolver is an
      unauthenticated uncapped work source.
- [ ] The cancel-on-full-buffer cliff (`lib.rs:1493-1495`, `:1564-1583`) is either given a
      backpressure alternative or documented as an accepted trade — a reader 256 events behind
      currently loses its turn's work.
- [ ] Failing-first tests per bound.

## Progress

- 2026-08-01 — filed from validation of SRV-01/SRV-02.

## Notes

- Pre-auth introspection amplification (`lib.rs:1202-1259`) is documented as a deployment
  responsibility and stays a design-decision; a preflight warning would still help.
