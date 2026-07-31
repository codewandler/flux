---
id: C-350
title: Establish a fail-closed confinement floor below the CLI
pillar: Core
status: backlog
epic: egress-pinning-and-confinement-residuals
design: docs/designs/egress-pinning-and-confinement-residuals.md
note: "C-262's guarantee is a flux-cli property; flux-sdk defaults to Off/network-open and grep -ri sandbox crates/flux-server/src returns ZERO hits — while os-sandbox.md:64 says EVERY serving surface uses require"
---

# Establish a fail-closed confinement floor below the CLI

## Goal

Make the unattended sandbox profile a property of the serving assembly rather than of one binary's
argument parser, or stop documenting it as though it already is.

## Acceptance

- [ ] An embedder standing up `flux-server` through `flux-sdk` resolves a fail-closed posture
      (`require` + closed sandbox network) by default, or `website/docs/security/os-sandbox.md:64`
      is corrected — "every HTTP/A2A serving surface automatically uses `require`" is true only of
      the CLI today.
- [ ] `flux app run <prog.flux>` with neither `--yes` nor `--serve` — a cron/webhook/Slack daemon —
      is classified as an unattended surface by `crates/flux-cli/src/dispatch.rs:6-28`, or the
      exclusion is recorded with its reasoning.
- [ ] The forgeable `FLUX_SANDBOXED` marker is audited wherever `Sandbox::resolve` accepts it
      (`crates/flux-system/src/sandbox.rs:186-193`), not only on the CLI path
      (`dispatch.rs:305-315`).
- [ ] Failing-first regression driving the SDK/server assembly with both sandbox discovery variables
      forced nonexistent, asserting it refuses to serve — the shape of
      `crates/flux-cli/tests/sandbox_posture.rs:237`.
- [ ] The sandbox truth table in the security docs is regenerated from the assembled surfaces and
      covers CLI, SDK embedder, `app run` daemon, `plugin call`, and `eval`.

## Progress

- 2026-08-01 — truth table re-derived during validation; the CLI half of SANDBOX-01 is genuinely
  fixed, every other assembly reproduces the reviewed posture.

## Notes

- `Sandbox::resolve` treating `AlreadyConfined` as satisfying `require` is correct; the gap is that
  only the CLI emits the `OUTER-CONFINEMENT` trust event when it does.
