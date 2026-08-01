---
id: C-442
title: "What do Codex, Claude Code, OpenCode and Pi document that we do not?"
pillar: Core
status: ready
priority: 6
design: docs/designs/docs-completeness.md
epic: docs-completeness
areas: [website, docs]
note: "⚠ verify against the LIVE docs; do not write this from recollection. A gap list assembled from memory of what a competitor's docs probably contain is exactly the confident-and-wrong artifact this repo keeps catching — leave a row empty rather than guess it"
---

# The things that are obvious to everyone except us

## Goal

A checked, sourced list of topics peer harnesses document that flux does not — each classified, so the
list turns into work rather than into a feeling.

## Why context management was only the first one

[C-441](C-441-context-management-doc.md) exists because a user noticed a missing page. The valuable
question is what **else** is obvious to everyone outside the project. Gaps of this kind are invisible
from the inside precisely because the adjacent pages exist and look complete.

## Method

Audit **Codex · Claude Code · OpenCode · Pi**, for **topics they document that flux does not**, and
classify each row:

- **missing page** — flux does the thing, nothing describes it;
- **covered but unfindable** — documented somewhere a reader will not look (the `FLUX_COMPACT_CHARS`
  pattern: a real entry in a 500-line config table);
- **deliberately absent** — flux does not do it, and the docs should say so rather than stay silent.

⚠ **Verify against the live docs.** Do not write a row from recollection of what a product "probably"
documents. If a source cannot be checked, leave the row empty and say which. A wrong claim about a
competitor is both embarrassing and, once it drives work, expensive.

⚠ **Compare topics, not tables of contents.** Copying a competitor's structure would import their
shape, and flux's genuinely differs — authored flows, a mandatory approval envelope, replay/fork/diff.
The question is *"does a reader leave with this question answered"*, not *"do we have a page with this
title"*.

## A starting hypothesis that is already in-repo

`docs/reviews/single/2026-08-01-pi-flux-harness-comparison.md` is a nine-axis rubric from two isolated
source-level reviews. Its two lowest relative scores are where to look first:

- **Operator UX / customization — Flux 8.0, Pi 9.0**, read as flux *"exposes richer safety and workflow
  controls at higher conceptual cost."*
- **Embeddability / automation — Flux 8.0, Pi 9.0**, read as flux *"asks more of the embedder."*

⚠ That review scored **code, not documentation**. But "higher conceptual cost" and "asks more of the
embedder" are exactly the burdens documentation exists to pay down — so treat them as a pointer, not as
a finding.

## Acceptance

- [ ] A sourced table: topic · which peers document it · flux's status · classification. Every non-empty
      row carries a link to the page it came from.
- [ ] ⚠ Unverifiable rows are **empty and marked**, not guessed.
- [ ] Each *missing page* row becomes a story, or is explicitly declined with a reason. A list nobody
      acts on was a reading exercise.
- [ ] *Deliberately absent* rows produce a **sentence in the docs saying so** — the reader who came
      looking is the one currently being failed, and silence reads as an oversight.
- [ ] The audit records its method well enough to be re-run when a peer ships new docs. ⚠ It will go
      stale; say when it was taken and against what.

## Notes

- Pairs with the [flux-recipes](../designs/flux-recipes.md) epic, which found the same shape from the
  other direction: a keyword sweep showed `agent_loop`, `await` and `datasource` had **zero** examples.
  Undemonstrated and undocumented are cousins.
- ⚠ Do not let this become a comparison table for marketing. C-429 already decided that positioning
  argues from the architecture and names no competitor. This is an **internal** audit whose output is a
  backlog, not a page.

## Progress

- Filed 2026-08-02 at the owner's request.
