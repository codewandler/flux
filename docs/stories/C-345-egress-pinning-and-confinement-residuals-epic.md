---
id: C-345
title: "Egress pinning and confinement residuals — pin every outer adapter, not just the reviewed ones (epic)"
pillar: Core
status: backlog
epic: egress-pinning-and-confinement-residuals
design: docs/designs/egress-pinning-and-confinement-residuals.md
note: "EPIC — NET-01/NET-02/PROC-01/PROC-02 all validate as historical-fixed, but the fix was applied per REVIEWED adapter; three unreviewed egress paths still resolve twice, one while carrying a credential"
---

# Egress pinning and confinement residuals

## Goal

Make connection-time address pinning and the fail-closed confinement floor properties of every
outer adapter and every assembly, rather than of the specific paths three reviewers happened to
read on 2026-07-30.

## Acceptance

- [ ] C-346 pins A2A push-notification delivery and corrects C-59's overstated closure claim.
- [ ] C-347 pins or explicitly bounds browser/CDP egress and makes the audit record agree with the
      address that was dialled.
- [ ] C-348 leaves no reachable unpinned egress API without a named, tested exemption.
- [ ] C-349 closes `core.fsmonitor` on the exempt git argv and re-states the I1 exemption reasons to
      name the full set of seams they actually close.
- [ ] C-350 gives `flux-sdk`/`flux-server` embedders and un-flagged `flux app run` daemons a
      fail-closed posture, or the documentation stops claiming they have one.
- [ ] C-351 bounds `eval_run`'s undeclared model-reachable parameters and binds it to the session's
      confinement rather than the host process's.
- [ ] An exemption inventory lists every outer adapter that resolves a hostname, each marked pinned
      or owned; a test fails when a new adapter appears outside the inventory.

## Progress

- 2026-08-01 — opened from the seven-way validation pass over `docs/reviews/aggregate/2026-08-01-aggregate-complaint-triage.md`.

## Notes

- The existing pinning regressions are strong: they reach real listeners through hostnames with no
  system DNS entry, so they cannot pass if pinning is removed. Match that bar for new adapters.
- `docs/stories/C-77-egress-dns-rebinding-pin.md` is `done` with an explicit `[~]` residual for the
  browser path and no follow-up story — C-347 is that follow-up.
