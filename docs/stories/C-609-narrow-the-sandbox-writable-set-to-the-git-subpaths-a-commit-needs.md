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

Delivered before contracts were validated; see this story's commits.

## Acceptance

- [x] `contract_waived` These criteria were never written. The story shipped, and inventing a contract for it now would be fiction rather than migration — what it actually delivered is in its commits. Recorded so `check` reports a known, reasoned gap instead of a silent one.
