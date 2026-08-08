---
id: C-750
title: "The acceptance parser is not fence-aware, so a story documenting the syntax corrupts its own contract"
pillar: "Core"
status: ready
priority: 3
epic: delivery-is-verified
areas: [flux-cli]
depends_on: [C-737, C-739, C-740]
design: docs/designs/story-contracts-are-validated.md
note: "section_contract decides its section by scanning `## ` headings with no fence awareness, so a heading inside a fenced block re-opens the section. C-739's own Syntax example gave the story a seventh criterion that could never be ticked. C-740 already solved this class with outside_code_spans"
---

# The acceptance parser is not fence-aware, so a story documenting the syntax corrupts its own contract

## Goal

`section_contract` decides which section a line belongs to by scanning for `## ` headings and
nothing else. It has no idea what a fenced code block is. So a `## Acceptance` line written *inside*
a fence re-opens the Acceptance section for the parser, and every checkbox after it is counted as a
real criterion.

This is not hypothetical and it is not rare in the place it matters most: **a story that documents
the board's own syntax has to show the syntax.** C-739's `## Syntax` section did exactly that, and
gave itself a seventh acceptance criterion — the example bullet — which no one could ever tick and
which made the story permanently unclosable by the very feature it introduced. It was found by hand
while closing the story, not by any check.

Everything downstream inherits it: `board done` refuses on a phantom `remaining`, `board stats`
inflates its totals, `board reconcile` derives `acceptance-complete` from a criterion that is an
illustration, and C-587's reviewer is handed a contract clause that was never a clause.

The fix already exists in the same file. C-740 needed exactly this distinction for its ambiguity
marker and wrote `outside_code_spans`, because C-740's own Acceptance quotes
`[NEEDS CLARIFICATION: ...]` and a literal scanner made that story unpromotable by its own rule.
The same reasoning applies one function over, and it was not carried across.

## Acceptance

- [ ] **Failing-first**: a fixture story whose `## Acceptance` section is followed by a `## Syntax`
      section containing a fenced block with a `## Acceptance` heading and an unticked box. Assert
      the story reports the criteria it really has, and that `board done` closes it. It must fail
      before the change.
- [ ] `section_contract` and `checkbox_counts` ignore headings and checkboxes inside fenced blocks,
      reusing C-740's `outside_code_spans` rather than growing a second notion of what a fence is.
- [ ] Indented `verify:` handles inside a fence are ignored for the same reason, per the risk C-739
      recorded when it shipped.
- [ ] The counts do not move for any story that contains no fenced block. Prove it over the corpus
      rather than asserting it: `board stats` `criteria.total` before and after must differ by
      exactly the criteria this fixes, and the story records that number.
- [ ] `board check` reports a story whose Acceptance section is re-opened by a fenced heading, so
      the shape is named rather than silently corrected — a story that meant to write two Acceptance
      sections should hear about it.
- [ ] Full gate green: `scripts/release-full-gate.sh`.

## Notes

- Found 2026-08-08 while closing C-739 in the batch integration. C-739's story was corrected in the
  same commit by dropping the heading line from inside its fence, so the repository is currently
  correct by convention rather than by construction — which is exactly the state this story exists
  to end.
- The blast radius is small today only because few stories document board syntax. That number goes
  up, not down: C-738 generates from `_TEMPLATE.md`, C-739 adds `AC-n` and `verify:`, C-740 adds the
  clarification marker and C-741 adds `kind:`. Every one of them is a syntax that stories will want
  to show.
- Related: [C-748](C-748-a-story-the-board-cannot-parse-is-a-failure-not-a-silent-skip.md) is the
  other half of "the parser's blind spots are invisible" — that one is about a file it cannot read
  at all, this one about a file it reads wrongly and confidently.
