---
id: C-609
title: "Narrow the sandbox writable set to the git subpaths a commit needs"
pillar: "Core"
status: done
epic: agent-evidence-scope
areas: [flux-system]
design: docs/designs/agent-evidence-scope.md
note: "linked_worktree_writable_roots grants the whole .git so a worker can write sibling session stores"
done_override: "Implemented in main: the sandbox writable set grants the admin dir plus only the shared git subpaths a commit writes — objects, refs, packed-refs, config (sandbox.rs:772, assertions at sandbox.rs:2443). Store isolation is recorded there as a relocation problem, not a fencing one."
---

# Narrow the sandbox writable set to the git subpaths a commit needs

## Goal


## Acceptance

- [ ] Define acceptance.
