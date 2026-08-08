---
id: C-611
title: "Enforce Fleet fences at path resolution, or rename them"
pillar: "Core"
status: ready
priority: 11
epic: agent-evidence-scope
areas: [flux-cli]
design: docs/designs/agent-evidence-scope.md
note: "the .git fence can never fire, .flux/fleet is checked against the wrong repository, template fences are dropped"
---

# Enforce Fleet fences at path resolution, or rename them

## Goal

A worker's write fence is the boundary between "an agent edited its story" and "an agent edited the
fleet that governs it". Three of the declared fences do not do what their names claim: the `.git`
fence can never fire, `.flux/fleet` is checked against the wrong repository, and template fences are
dropped entirely. A fence that cannot fire is worse than no fence, because the contract is written
down and believed.

## Acceptance

- [ ] Every declared fence is enforced at the point a path is resolved, so a fence cannot be bypassed
      by reaching the same file through a different path.
- [ ] The `.git` fence fires. Prove it with a worker attempting a write inside `.git` and being
      refused.
- [ ] `.flux/fleet` is checked against the repository that owns the fleet, not the member under work.
- [ ] Template fences reach the worker they were declared for; a dropped fence is a hard error at
      admission rather than silence.
- [ ] Any fence that cannot be enforced is removed from the vocabulary rather than left declared —
      the contract must not claim a protection that does not exist.
- [ ] Regression test: a worker refused at each fence, and a test that fails if a declared fence has
      no enforcement site.
