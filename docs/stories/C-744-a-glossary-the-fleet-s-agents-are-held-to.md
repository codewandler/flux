---
id: C-744
title: "A glossary the fleet's agents are held to"
pillar: "Core"
status: backlog
priority: 4
epic: delivery-is-verified
areas: [docs]
---

# A glossary the fleet's agents are held to

## Goal

The fleet's vocabulary is dense and precise — wave, park, claim, harvest, capture, fence, canonical
ref, member, lane, milestone, candidate, admitted operation — and agents demonstrably mangle it.
`AGENTS.md` keeps correcting the same confusions in prose: "a worker handoff is not Board
completion", "`already-built` matches a mention, not an implementation". Prose corrections do not
survive contact with a fresh agent; a checked glossary might.

## Acceptance

- [ ] A glossary defines every term the board and fleet contracts depend on, in one place, with the
      distinction that makes each term non-obvious.
- [ ] It is part of what an agent reads before acting, not a document it could skip.
- [ ] A lint flags near-miss synonyms across stories — wave versus batch, park versus block, claim
      versus lock — because a story that renames a concept is how the vocabulary drifts.
- [ ] The glossary changes in the same commit as a rename, so it cannot lag the code it describes.
