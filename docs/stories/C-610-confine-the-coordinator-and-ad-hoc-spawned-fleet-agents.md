---
id: C-610
title: "Confine the coordinator and ad-hoc spawned Fleet agents"
pillar: "Core"
status: done
epic: agent-evidence-scope
areas: [flux-cli]
design: docs/designs/agent-evidence-scope.md
note: "main and fleet spawn take the entire fleet root as writable plus every repository root as read roots"
done_override: "Implemented and tested in main: the coordinator and ad-hoc fleet agents record their capability set at start; test the_coordinator_records_every_root_and_a_worker_records_exactly_one asserts the asymmetry (3 roots/18 ops vs 0 roots/43 ops)."
---

# Confine the coordinator and ad-hoc spawned Fleet agents

## Goal

Delivered before contracts were validated; see this story's commits.

## Acceptance

- [x] `contract_waived` These criteria were never written. The story shipped, and inventing a contract for it now would be fiction rather than migration — what it actually delivered is in its commits. Recorded so `check` reports a known, reasoned gap instead of a silent one.
