---
id: C-738
title: "Creating a story generates the template and refuses a ready story with no contract"
pillar: "Core"
status: backlog
priority: 2
epic: delivery-is-verified
areas: [flux-cli]
---

# Creating a story generates the template and refuses a ready story with no contract

## Goal

`create_item` writes a hardcoded body that does not match `docs/stories/_TEMPLATE.md` — no
`## Progress`, no `## Notes`, an empty `## Goal`, and `- [ ] Define acceptance.` as the sole
criterion. The template exists and is ignored. A story can also be created directly at `ready`,
which makes it dispatchable while its contract says "Define acceptance."

## Acceptance

- [ ] A new planning document is generated from `docs/stories/_TEMPLATE.md`, so the template is the
      one definition of a story's shape rather than a second one that drifts.
- [ ] `create --status ready` refuses unless the story has a real Goal and at least one criterion —
      a story cannot be born dispatchable and empty.
- [ ] Creating at `backlog` still allows a placeholder, because drafting has to be possible; the
      `backlog -> ready` transition is where the contract becomes mandatory.
- [ ] Regression test: `create --status ready` with no contract is refused, `create` at backlog
      produces a body matching the template, and the template and generated body cannot diverge
      without a test failing.
