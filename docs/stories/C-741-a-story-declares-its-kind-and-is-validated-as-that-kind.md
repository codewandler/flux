---
id: C-741
title: "A story declares its kind and is validated as that kind"
pillar: "Core"
status: backlog
priority: 3
epic: delivery-is-verified
areas: [flux-cli]
---

# A story declares its kind and is validated as that kind

## Goal

Every story is validated as though it were a feature. A spike's output is a decision, an enabler's is
capability, a bug's contract is current-versus-expected behaviour — judging any of them against
"behaviour implemented with a failing-first test" is a schema lie, and it pushes authors toward
writing criteria they do not mean.

## Acceptance

- [ ] A story declares `kind: feature | enabler | spike | bug`, defaulting to `feature` so existing
      stories are unaffected.
- [ ] Validation follows the kind. A spike is not required to name a failing-first test; a bug states
      current and expected behaviour; an enabler names the capability it unlocks.
- [ ] The kind is visible to the driver, so dispatch and review apply the rules that fit the work.
- [ ] Regression test: a spike with no failing-first test passes `check`, and a feature without one
      does not.
