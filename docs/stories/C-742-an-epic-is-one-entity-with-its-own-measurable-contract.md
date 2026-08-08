---
id: C-742
title: "An epic is one entity with its own measurable contract"
pillar: "Core"
status: backlog
priority: 3
epic: delivery-is-verified
areas: [flux-cli]
---

# An epic is one entity with its own measurable contract

## Goal

An epic is currently **three inconsistent things**: a free-text `epic:` string on 1048 stories that
is never resolved to anything (~39 distinct slugs point at no document, silently); a design doc
written by `create --kind epic` with no frontmatter, no id, no status and no acceptance, whose `E-`
id is allocated and then discarded; and 58 ordinary story files that are epic trackers by convention.
No rule anywhere distinguishes an epic from a story, so `check` applies leaf-story rules to `C-420`.

## Acceptance

- [ ] One representation. An epic is a single entity with an id, a status and its own contract, and
      the other two forms are migrated onto it.
- [ ] `epic:` resolves, the way `design:` already does — a story naming an epic that does not exist
      is an error, not silence.
- [ ] An epic carries measurable success criteria and exit criteria, so it is not merely a bag of
      stories. Its completion is derived from its stories rather than asserted.
- [ ] Regression test: a story naming a nonexistent epic fails `check`, and an epic's completion is
      computed from its members.
