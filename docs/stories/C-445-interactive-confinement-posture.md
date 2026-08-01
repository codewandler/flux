---
id: C-445
title: "Interactive turns are deliberately exempt from OS confinement — re-take that decision with the finding in hand"
pillar: Core
status: ready
priority: 5
design: docs/designs/pi-comparison-remediation.md
epic: pi-comparison-remediation
areas: [flux-cli, flux-system]
note: "the precise remaining secure-defaults gap after C-410: unattended CLI is fail-closed, interactive is not, and `even installed plugin startup can run unconfined` (dispatch.rs:111). Deliberate is a decision that should be RE-TAKEN, not inherited"
---

# The last surface that runs unconfined

## Goal

Decide, with the finding in front of us, whether interactive turns should stay exempt from OS
confinement — and implement whichever answer wins.

## What the review says, precisely

> *"The CLI now raises unattended, auto-approved and serving surfaces to fail-closed `require` and
> defaults their sandbox network closed. Interactive turns remain deliberately exempt — even installed
> plugin startup can run unconfined"* (`crates/flux-cli/src/dispatch.rs:111`).

And its summary: *"unattended CLI execution is fail-closed by default, but interactive and SDK usage
still require an explicit OS isolation decision."*

⚠ The exemption is **deliberate** — a human is present, and confinement costs interactive
capability. That was decided before C-410 closed the unattended surfaces, which changes the shape of
the argument: the exemption is now the *only* CLI path that runs unconfined.

## Acceptance

- [ ] The decision is re-taken and **recorded with its reasoning**, whichever way it goes. "It has always
      been exempt" is not a reason.
- [ ] ⚠ **The plugin-startup case is decided separately.** *Installed plugin startup running unconfined*
      is not the same as *a human's interactive turn* — a plugin binary starts without the human having
      asked for anything in particular, which is much closer to the unattended posture C-410 closed.
- [ ] If the exemption stays, the docs say **exactly which surfaces run unconfined and why**, where an
      operator will read it — not only in a design doc.
- [ ] If it goes, the interactive capability cost is measured rather than assumed, and there is an
      escape hatch.
- [ ] Full gate green.

## Notes

- ⚠ Do not let this be settled by whichever is easier to implement. Both answers are defensible; only
  an unexamined one is not.
- `crates/flux-system/src/sandbox.rs:35` — the underlying sandbox is off with open network when nothing
  requests it. That default is what every unclassified surface inherits.

## Progress
- Filed 2026-08-02 from the Pi comparison.
