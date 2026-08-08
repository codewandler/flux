---
id: C-733
title: "A member's canonical_ref must be a ref the fleet can write"
pillar: "Core"
status: backlog
priority: 2
epic: delivery-is-verified
areas: [flux-orchestrate]
note: "connectors and exchange declare canonical_ref = origin/main, a remote-tracking ref no fleet operation can write. apply cannot reach it, promote refuses it by name, and C-721 reports both members' waves as applied-without-delivery. Decision 0021 section 2 says board validation must refuse the combination rather than accept it and silently under-deliver. Needs the config change plus the validation, and the website example config carries the same mistake"
---

# A member's canonical_ref must be a ref the fleet can write

## Goal


## Acceptance

- [ ] Define acceptance.
