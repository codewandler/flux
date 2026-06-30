---
id: X-NN              # pillar letter (A=Agent, L=Language, I=Improve, C=Core/Shared) + number
title: Short imperative title
pillar: Agent         # Agent | Language | Improve | Core
status: backlog       # backlog | ready | in-progress | blocked | done
priority:             # integer rank among `ready` stories; omit otherwise
epic:                 # optional: design-doc slug; the board groups backlog/ready rows under it
design:               # optional: docs/designs/<slug>.md for non-trivial work
note:                 # optional: one-line annotation rendered on the board row (never a secret)
---

# <title>

## Goal
One or two sentences: the outcome this delivers and which pillar value it serves.

## Acceptance
- [ ] A testable criterion. A behavioral change must name the failing-first test that proves it.
- [ ] …

## Progress
- (running log / checklist — a resuming agent reads this to know exactly where things stand)

## Notes
- Links, blockers, design pointers, relevant files.
