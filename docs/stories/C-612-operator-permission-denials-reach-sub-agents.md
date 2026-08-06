---
id: C-612
title: "Operator permission denials reach sub-agents"
pillar: "Core"
status: done
epic: agent-evidence-scope
areas: [flux-orchestrate]
design: docs/designs/agent-evidence-scope.md
note: "a child executor gets an empty PermissionManager and no disabled ops, so deny and tools.disable stop at delegation"
done_override: "Implemented and tested in main (8728936e): operator permission rules are carried into every sub-agent and descend through nesting (flux-orchestrate/src/lib.rs:158, :219, :346). NOTE: the fleet separately re-implemented this in wave-385 because this story still read `ready` — that duplicated work is the cost of the gap this transition closes."
---

# Operator permission denials reach sub-agents

## Goal


## Acceptance

- [ ] Define acceptance.
