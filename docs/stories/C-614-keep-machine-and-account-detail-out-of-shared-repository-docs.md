---
id: C-614
title: "Keep machine and account detail out of shared repository docs"
pillar: "Improve"
status: ready
priority: 14
epic: agent-evidence-scope
areas: [docs]
design: docs/designs/agent-evidence-scope.md
note: "a committed skill carried this host's paths, a credentials file location and the operator's billing state"
---

# Keep machine and account detail out of shared repository docs

## Goal

Anything committed here is shared. A committed skill carried this host's absolute paths, the location
of a credentials file, and an assertion about the operator's billing state — none of which is true of
anyone else's machine, and one of which is a pointer at a secret. `AGENTS.md` already states the rule
("describe the mechanism, never this machine or this account"); nothing enforces it.

## Acceptance

- [ ] Committed documentation and skills carry no absolute path from a developer's home directory, no
      credential file location, and no assertion about a particular account's balance, quota or
      billing state.
- [ ] The rule is enforced by a check that runs in the gate, not by review attention — the existing
      violations reached `main` past review.
- [ ] A reproduction keeps the command and the error and drops the operator's environment, so the
      evidence survives redaction.
- [ ] The current violations are removed, and the check fails on the tree before that removal.
- [ ] Regression test: a fixture document carrying each shape is refused, and one describing the same
      mechanism generically passes.
