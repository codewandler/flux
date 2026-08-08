---
id: C-613
title: "Give Workspace a narrowing operation and carry it on delegation"
pillar: "Core"
status: done
epic: agent-evidence-scope
areas: [flux-system]
design: docs/designs/agent-evidence-scope.md
note: "extends D-05 in the one direction it never needed: a child that should see less than its parent"
done_override: "Implemented and tested in main: Workspace gained a strictly-narrowing constructor (flux-system/src/lib.rs:321) with a failing-first test at lib.rs:3060."
---

# Give Workspace a narrowing operation and carry it on delegation

## Goal

Delivered before contracts were validated; see this story's commits.

## Acceptance

- [x] `contract_waived` These criteria were never written. The story shipped, and inventing a contract for it now would be fiction rather than migration — what it actually delivered is in its commits. Recorded so `check` reports a known, reasoned gap instead of a silent one.
