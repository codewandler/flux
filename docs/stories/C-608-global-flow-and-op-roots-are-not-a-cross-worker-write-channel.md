---
id: C-608
title: "Global flow and op roots are not a cross-worker write channel"
pillar: "Core"
status: done
epic: agent-evidence-scope
areas: [flux-cli]
design: docs/designs/agent-evidence-scope.md
note: "@named roots are write-capable; write to @global_flows needs only read+edit and is invisible to every diff"
done_override: "Implemented and tested in main: workspace_with_flow_roots_scoped withholds the host-global @named roots from a ceiling-confined agent (execution.rs:1333), wired at execution.rs:2230, failing-first test at execution.rs:1893."
---

# Global flow and op roots are not a cross-worker write channel

## Goal


## Acceptance

- [ ] Define acceptance.
