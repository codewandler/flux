---
id: C-265
title: "Keep built-in strict review immutable, toolless, and fail-closed"
pillar: Core
status: done
epic: adversarial-review-remediation-2026-07-30
design: docs/designs/strict-review-flows.md
areas: [flux-cli, flux-app, flux-orchestrate]
note: "HIGH closure review — project role shadowing turned a promised read-only auto-approved command into workspace-write authority"
---

# Keep built-in strict review immutable, toolless, and fail-closed

## Goal

Make the built-in `flux review` protocol safe to invoke in an untrusted checkout: repository role
files cannot replace its embedded reviewers, its children have no tools, and its auto-approved
execution starts only under the unattended sandbox posture.

## Acceptance

- [x] A regression fixture with a project `review-security` role requesting `write` proves ordinary
      role discovery still sees the override while the built-in review protocol ignores it.
- [x] `flux review` and the built-in `strict-review` app share one immutable registry containing
      exactly the three embedded `tools: []` reviewer roles.
- [x] A real-binary sandbox test proves `flux review` fails before provider/reviewer work when no
      confinement backend is available.
- [x] Strict-review design and product documentation no longer advertise project role replacement
      as part of the built-in security protocol.
- [x] Scoped CLI/app tests, format, Clippy, codegate, and release/assurance policy checks are green.

## Progress

- 2026-07-30 — filed from the first fresh closure review after C-255. The review command loaded
  project roles into a full built-in child registry; the default headless sub-agent approver then
  allowed a shadow role's non-destructive `write` call while CLI startup left `review` outside the
  unattended sandbox classifier.
- 2026-07-30 — built-in review now constructs its role registry only from embedded role sources and
  classifies direct review as unattended. Failing-first role-shadowing and real-binary confinement
  regressions pass, along with CLI/app Clippy, format, codegate, release/assurance policy self-tests,
  action pins, and changelog/docs mirrors.

## Notes

- User-defined review criteria remain tracked separately by C-161. They must compose through a
  future typed, read-only extension seam rather than replacing these security-critical role names.
