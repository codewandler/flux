---
id: C-511
title: "Label the Exchange environment token as transitional"
pillar: Core
status: done
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "C-503 compatibility must not be mistaken for C-509's secure first-run bootstrap"
---

# Label the Exchange environment token as transitional

## Goal

Keep the shipped C-503 environment-token compatibility seam honest: it proves the embedded client,
but it is not the Milestone 1 onboarding contract and C-509 replaces it with an Exchange-owned direct
handoff into secure storage.

## Acceptance

- [x] README, configuration reference, ecosystem direction and the assembly comment call
      `FLUX_EXCHANGE_SERVICE_ACCOUNT_TOKEN` transitional C-503 compatibility and name C-509's
      secure-store replacement wherever they teach that setup.
- [x] The current C-503 runtime seam and its redaction/non-leak behavior remain unchanged; this
      correction neither removes it early nor presents an environment bearer as final onboarding.
- [x] A failing-first documentation contract rejects any affected public setup surface that names
      the environment token without both its transitional status and C-509 replacement, and the
      generated ecosystem mirror remains exact.

## Progress

- 2026-08-04: filed after the independent C-503 re-audit found PR #13's implementation and security
  proof sound but its public setup wording inconsistent with the already-merged C-509 contract.
- 2026-08-04: the new documentation contract failed first on README, then passed across every named
  surface; the ecosystem golden was regenerated under the armed workflow and the complete unarmed
  website-sync suite passed.

## Notes

- C-509 owns removal of the environment-token bootstrap after its direct secure-store handoff lands.
- This story changes documentation and explanatory comments only; it does not widen the bearer seam.
