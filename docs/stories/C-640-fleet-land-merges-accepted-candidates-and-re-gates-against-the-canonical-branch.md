---
id: C-640
title: "fleet land merges accepted candidates and re-gates against the canonical branch"
pillar: "Core"
status: ready
priority: 14
epic: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
areas: [flux-cli]
design: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
note: "acceptance pins a candidate against the base it was gated on; landing is a separate act needing its own verification, and it is a bash approximation in the driver"
---

# fleet land merges accepted candidates and re-gates against the canonical branch

## Goal

Landing an accepted candidate on the canonical branch is a separate act from
accepting it, and needed its own verification rather than the driver's bash approximation.
Delivered as `flux fleet promote` under C-681/C-732.

## Acceptance

- [x] Superseded by [[C-681]], which shipped `flux fleet promote`: it accumulates each member's
      accepted candidates, gates them in a throwaway worktree against the canonical ref, and lands
      by compare-and-swap `update-ref`. C-732 then made the drive tick call it. This story's work
      exists; it was done under another id.
