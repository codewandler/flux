---
id: C-742
title: "An epic is one entity with its own measurable contract"
pillar: "Core"
status: in-progress
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

- [x] One representation. An epic is a single entity with an id, a status and its own contract, and
      the other two forms are migrated onto it.
- [x] `epic:` resolves, the way `design:` already does — a story naming an epic that does not exist
      is an error, not silence.
- [x] An epic carries measurable success criteria and exit criteria, so it is not merely a bag of
      stories. Its completion is derived from its stories rather than asserted.
- [x] Regression test: a story naming a nonexistent epic fails `check`, and an epic's completion is
      computed from its members.

## Progress

### The representation kept, and why

**The `epic:` slug stays as the reference; the entity it now refers to is a new document type at
`docs/epics/<slug>.md`.** Of the three forms, exactly one had a property worth keeping from each, and
the choice was between fixing form (2) in place and giving the entity its own root:

- **Form (1), the slug**, is what `check`, `render_grouped` and `stats` already consume, and it is
  carried by 991 stories. Rewriting those into paths would be churn with no reader, so the slug is
  untouched — it stopped being free text by acquiring something to resolve *to*, not by changing
  shape. **Zero story files were edited by this change.**
- **Form (2), the `create --kind epic` design document**, was rejected as the identity holder. Its
  problem is not only the missing frontmatter: `docs/designs/` holds 197 files of which 109 are
  epic-shaped and the rest are designs for ordinary stories, so "an epic is the subset of designs
  that happens to carry frontmatter" reproduces the undecidability being removed. It is migrated by
  *reference* instead — the epic's `design:` field points at it, and `epic_title`/`epic_blurb` now
  prefer the epic document and fall back to the design.
- **Form (3), the 56 `*-epic.md` tracker stories**, was rejected as the entity because its defect is
  exactly that it is indistinguishable from a leaf story, and because only 56 of 137 slugs had one.
  It is migrated by *recorded mapping* — the epic's `tracker:` field names it, `check` verifies the
  id exists, and the tracker file survives as the narrative record. Where a tracker stated an
  explicit `## Acceptance (for the epic)`, that contract was lifted into the epic document verbatim.

The decisive argument for a new root over editing 109 design documents in place: creating files
cannot break an existing reference or collide with a concurrent writer, and five sibling stories were
rebasing across `board_fleet_cmd.rs` at the time.

### An epic never declares its completion

`check` **refuses** a `status:` field on an epic document. The old trackers sat at `status: backlog`
with every member `done` and nothing could tell; a field the board silently discards is how that
happened. `epic_progress` derives the status from the member set and `flux board epics` reports it.
This is a deliberate reading of "an id, a status" in the first Acceptance item: the entity has a
status, and it is computed rather than written down.

### Migration of the dangling slugs

Measured on this tree, not the Goal's estimate: **137 distinct slugs in use across 991 stories, of
which 28 resolved to no `docs/designs/<slug>.md` at all** (143 stories). The Goal says ~39; that
number predates some designs landing. Turning resolution on would have failed `check` for all 143, so
all 137 slugs got a document in the same change — `check` is green on this tree.

Each document derives its content, never invents it: the title comes from the tracker story (with a
trailing `(epic)` stripped) or the design's `# ` heading or a title-cased slug; `design:` and
`tracker:` are set only where the file actually exists. Exit criteria are the two genuinely derived
rules. Success criteria are hand-authored for **4** epics where real material existed
(`delivery-is-verified` written fresh; `connector-platform`, `verified-webhook-channel` and
`network-primitives` lifted from their trackers' `## Acceptance (for the epic)`), and the remaining
**133** carry a `[NEEDS AUTHORING]` marker that `check` counts and reports as one aggregated warning.
Seeding a plausible-looking criterion would have made `board epics` report a contract nobody agreed
to; an admission a tool counts is the honest form of that debt.

### Left for a follow-up

- Authoring the 133 seeded contracts. The 52 remaining tracker stories carrying a plain
  `## Acceptance` are the obvious source, but lifting them means editing story files this change
  deliberately did not touch.
- `epic_title` now prefers the epic document, so the board's `###` epic headings will change text on
  the next `flux board sync`. The board is regenerated at integration, not here.
