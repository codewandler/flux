---
id: C-369
title: "Serving surface and turn outcome residuals — the ingress nobody inventoried (epic)"
pillar: Core
status: backlog
epic: serving-surface-and-turn-outcome-residuals
design: docs/designs/serving-surface-and-turn-outcome-residuals.md
note: "EPIC — SRV-01/SRV-02/OUTCOME-01 are all genuinely fixed in flux-server, and all three reopen one layer out: the webhook/connector channel adapters mount a bare Router with no limits and spawn before admission, and turn_end.outcome still reports suspended/max_iter/cancelled as ok"
---

# Serving surface and turn outcome residuals

## Goal

Extend the limit contract and the honest-outcome contract to every surface that accepts outside
bytes and starts a turn, not only the routes three reviewers read.

## Acceptance

- [ ] C-370 brings the `webhook` and `connector` channel adapters under the server limit contract.
- [ ] C-371 bounds queued work at the turn gate.
- [ ] C-372 gives every long-lived stream a ceiling and a permit.
- [ ] C-373 reports the durable turn outcome vocabulary on the wire.
- [ ] C-374 makes the stage-failure carry total rather than best-effort.
- [ ] Each ingress class is load-tested with valid credentials and returns a typed limit response.

## Progress

- 2026-08-01 — opened from validation. `dropping_rest_sse_body_cancels_and_finalizes_the_turn` was
  executed during the pass and passes; the C-260/C-261/C-226 fixes are real.

## Notes

- Severity note preserved from the ledger: P rated the daemon rate-limit gap Low and A/B rated it
  Medium. P was right about `flux-server`; A/B are right about the channel adapters.
