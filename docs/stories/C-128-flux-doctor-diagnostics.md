---
id: C-128
title: flux doctor — environment & install diagnostics command
pillar: Core
status: backlog
priority:
epic:
design:
note: "one command that checks credentials/OAuth expiry, plugin hash drift (D-48 machinery), sandbox backend availability, events.db + WAL health, egress config sanity, and version skew — each with a fix-it hint; cheap (every check exists as an internal predicate), high leverage for external-beta users"
---

# flux doctor — environment & install diagnostics command

## Goal
One `flux doctor` command that diagnoses a flux install end-to-end and prints actionable fix-it
hints, so external users (the flux-qa beta audience) can self-serve instead of filing
"it doesn't work" reports.

## Acceptance
- [ ] `flux doctor` runs a check suite and reports pass/warn/fail per check with a one-line fix-it
  hint on every non-pass; exit code non-zero iff any check fails.
- [ ] Checks cover at minimum: credential-store entries per configured provider (incl. OAuth token
  expiry), plugin pack signature/hash drift (reusing the D-48 verification), sandbox backend
  availability (bwrap / sandbox-exec probe), `events.db` integrity + WAL size, egress/private-net
  config sanity, and version skew vs the latest release.
- [ ] Every check is hermetic-testable: each has a unit test driving its pass and fail branches
  without live network/credentials (failing-first for the command itself).
- [ ] `--json` output for scripting.

## Progress
- (not started — filed from the 2026-07-28 feature-suggestion pass)

## Notes
- Most checks already exist as internal predicates; this story is mostly assembly + presentation.
