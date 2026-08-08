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


## Acceptance

- [ ] Define acceptance.
