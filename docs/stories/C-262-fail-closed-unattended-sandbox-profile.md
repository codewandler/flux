---
id: C-262
title: "Make unattended execution fail closed on sandbox and network posture"
pillar: Core
status: done
priority: 7
epic: adversarial-review-remediation-2026-07-30
design: docs/designs/adversarial-review-remediation-2026-07-30.md
areas: [flux-system, flux-cli, flux-server]
note: "MEDIUM — default process posture is unconfined and network-open; `on` may degrade and Windows has no backend"
---

# Make unattended execution fail closed on sandbox and network posture

## Goal

Keep interactive development usable while ensuring an unattended or serving deployment cannot
silently start with the exact unconfined/network-open posture all three reviews rejected.

## Acceptance

- [x] Define one explicit unattended profile covering `--yes` non-interactive runs and serving
      surfaces; it requires an active backend and denies sandbox network by default.
- [x] Failing-first integration tests prove those surfaces currently start with sandbox `off`, then
      prove startup fails before provider/tool work when confinement is unavailable.
- [x] An explicit, prominent operator escape hatch can request unconfined operation, but it is never
      inferred from an unknown environment value and is recorded in startup/audit posture.
- [x] Interactive local operation retains an honest documented mode; `on` versus `require` semantics
      and the C-217 disclosure remain consistent.
- [x] Windows/unsupported platforms fail the unattended profile with an actionable outer-VM/container
      requirement rather than claiming confinement.
- [x] CLI/server/system tests, public docs, WHATS-NEW, and the standard gate are green.

## Progress

- Added a CLI startup profile for auto-approved noninteractive `run`, `fork`, `record`, `flow run`,
  `preset --run`, and `app run`, plus every HTTP/A2A serving invocation. The profile raises sandbox mode to
  `require` and closes sandbox network unless the operator explicitly opens it.
- Kept the REPL, normal local runs, and the TUI on the existing interactive `off`/`on`/`require`
  contract, including C-217's soft-`on` disclosure.
- Made `--no-sandbox` and exact `FLUX_SANDBOX=off` the only unattended *unconfined* escapes; each
  emits a source-attributed `UNCONFINED` startup audit warning requiring equivalent outer
  container/VM isolation. A truthy `FLUX_SANDBOXED` marker remains the nested-process assertion that
  an outer boundary exists, but accepting it now emits a prominent `OUTER-CONFINEMENT` audit warning
  because the child cannot independently verify ambient process state. Unknown values cannot select
  either posture.
- Added real-binary fail-closed/escape tests and unit coverage for network defaults, unsupported
  backends, unknown environment values, inherited-confinement audit, `preset --run --yes`, and
  interactive compatibility.
- A closure review found the internally auto-approved `flux review` command missing from the
  classifier. It now inherits `require` plus closed sandbox network, and the real-binary regression
  proves an unsupported host fails before any reviewer/provider work.
- Targeted CLI, server, system, Clippy, and codegate checks pass. WHATS-NEW and its website mirror
  are synchronized, and the integrated workspace build/test/Clippy/format gate is green.

## Notes

- This is step 2 after completed C-217. It does not require inventing an unsafe fake Windows sandbox.
