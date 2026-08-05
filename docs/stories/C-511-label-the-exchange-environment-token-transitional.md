---
id: C-511
title: "Label the Exchange environment token as transitional"
pillar: Core
status: done
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "C-509 replaces the bearer only for managed Linux-local bootstrap; configured remote attach remains transitional on every Flux target"
---

# Label the Exchange environment token as transitional

## Goal

Keep the shipped C-503 environment-token compatibility seam honest: it proves the embedded client,
but it is not the managed Linux-local Milestone 1 onboarding contract. C-509 replaces it there with
an Exchange-owned direct handoff into secure storage; the configured origin/bearer remains the only
current remote attach seam on every Flux target until secure remote provisioning is separately
contracted.

## Acceptance

- [x] README, configuration reference, ecosystem direction and the assembly comment call
      `FLUX_EXCHANGE_SERVICE_ACCOUNT_TOKEN` transitional C-503 compatibility, name C-509's
      Linux-local secure-store replacement and retain the independently provisioned remote attach
      seam on every Flux target wherever they teach that setup.
- [x] The current C-503 runtime seam and its redaction/non-leak behavior remain unchanged; this
      correction neither removes it early nor presents an environment bearer as final local or
      remote onboarding.
- [x] A failing-first documentation contract rejects any affected public setup surface that names
      the environment token without both its transitional status and topology-scoped replacement,
      and the generated ecosystem mirror remains exact.

## Progress

- 2026-08-04: filed after the independent C-503 re-audit found PR #13's implementation and security
  proof sound but its public setup wording inconsistent with the already-merged C-509 contract.
- 2026-08-04: the new documentation contract failed first on README, then passed across every named
  surface; the ecosystem golden was regenerated under the armed workflow and the complete unarmed
  website-sync suite passed.
- 2026-08-05: Decision 0012 scoped C-509's replacement to managed Linux-local bootstrap and retained
  the transitional remote runtime attach seam on every Flux target. Shipped behavior did not change.

## Notes

- Decision 0012 narrows C-509's direct secure-store replacement to supported Linux local onboarding.
  The transitional configured origin/bearer remains the remote runtime seam on every Flux target,
  including Linux, until a separately contracted secure remote provisioning flow lands.
- This story changes documentation and explanatory comments only; it does not widen the bearer seam.
