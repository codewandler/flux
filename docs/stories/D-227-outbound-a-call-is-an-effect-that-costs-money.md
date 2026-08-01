---
id: D-227
title: "Outbound — placing a call bills money and rings a human, so it is an approval-gated effect with a destination allowlist"
pillar: Agent
status: ready
priority: 8
design: docs/designs/sip-channel.md
epic: sip-channel
areas: [flux-channels, flux-policy]
note: "⚠ the toll-fraud story. A model that can choose a dialled number is a premium-rate fraud vector, and the failure is financial and fast. The destination allowlist is the telephone analogue of guard_url_scoped — and must not be softened for ergonomics"
---

# Dialling is not a read

## Goal

flux places an outbound call only through the approval envelope, and only to an allowlisted
destination.

## ⚠ Why this one cannot be relaxed

An outbound call is not a retrieval. It **bills**, and it **makes a phone ring in someone's hand**.
Both consequences are irreversible in the way that matters: money is spent and a person is interrupted.

**A model that can choose the dialled number is a premium-rate toll-fraud vector.** International and
premium ranges bill at a rate that turns a loop bug into a large invoice inside an hour. The
destination allowlist is the telephone analogue of `guard_url_scoped`: flux already refuses to reach an
arbitrary host on the model's say-so, and a dialled number deserves the same treatment.

## Acceptance

- [ ] **Failing-first**: a test asserting an outbound call to a non-allowlisted destination is
      **refused**, and that an allowlisted one still requires approval — failing at the merge base.
- [ ] The op is classified destructive/approval-gated so it is forced to human approval **even under
      permissive rules**, which is the treatment `AGENTS.md` already prescribes for destructive effects.
- [ ] ⚠ **A destination allowlist, default-deny.** Not a blocklist — a blocklist of premium ranges is a
      moving target maintained by adversaries.
- [ ] ⚠ **Normalization is part of the check, not before it.** `+49…`, `0049…`, `00 49…` and a dial-plan
      prefix are the same destination; an allowlist that matches on unnormalized text is a bypass
      wearing a whitelist. Pin the equivalent-spellings case with a test.
- [ ] A rate/spend bound exists, so a loop cannot place calls without limit even to allowlisted
      destinations. ⚠ Approval is per-call and a runaway loop can request many.
- [ ] The approval disclosure names the **actual destination that will be dialled** after normalization,
      not the string the model proposed — this is the same lesson as C-311's vendor-host disclosure.
- [ ] Full gate green.

## Notes

- Settleable ahead of [D-225](D-225-the-sip-sidecar-seam.md); the policy does not need the transport.
- ⚠ Read C-311 (vendor-host disclosure at approval) first. It is the closest existing analogue: an
  approval that names something other than what will actually be reached is worse than none, because it
  is trusted.
- An emergency-services number reached by accident is a real-world harm, not a theoretical one. Whether
  those are refused outright is a decision this story should make rather than inherit.

## Progress

- Filed 2026-08-01 with the sip-channel epic.
