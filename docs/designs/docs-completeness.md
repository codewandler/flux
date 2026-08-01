# Design: The docs are missing things a user expects to find — starting with context management

**Status:** proposed · **Pillar:** Core · **Stories:** [C-441](../stories/C-441-context-management-doc.md) · [C-442](../stories/C-442-peer-docs-gap-audit.md) · [C-443](../stories/C-443-zero-compacted-rows.md)

## Why

Someone evaluating an agent harness arrives with a checklist they did not write down. *How does it
handle a long conversation? What happens when the context fills? Can I control it?* Those questions get
asked of every coding agent, and flux's docs do not answer them.

**The gap is concrete, not a feeling.** Compaction is implemented — `compact_threshold_chars` on the
engine (`0` disables it), `maybe_compact` in the loop host, and an `EventKind::Compacted { messages }`
that **replaces** history in the durable log. Its entire user-facing documentation is **one row in a
config table**: `FLUX_COMPACT_CHARS — "Character threshold that triggers history compaction."`

Meanwhile `website/docs/language/context-packs.md` documents `ctx` — the *Flux-Lang* construct — and
`website/docs/agent/project-context.md` documents project context. Both are real and neither answers
the question a user is actually asking, which makes the gap easy to miss from the inside: it looks
covered.

⚠ And a replaced history is not a detail. It changes what a session *is* — for replay, for
`flux export`, and for anything reconstructing a run from the log. [C-422](../stories/C-422-the-render-projection.md)
already had to make compaction an open question ("pre- or post-compaction view?"), and
[A-145](../stories/A-145-a-real-run-as-the-mock-fixture.md) could not build a fixture for it at all.

## ⚠ The finding underneath the docs gap

A-145 swept the event store to build a real-run fixture and reported: **zero `Compacted` rows in
112,114 events.**

That is not a documentation problem. Either the threshold is set so high that real sessions never reach
it, or compaction is effectively disabled in practice, or it fires and does not record. Any of the three
means the feature is undocumented *and* unexercised — and writing a confident page about behaviour
nobody has observed would be worse than the silence. [C-443](../stories/C-443-zero-compacted-rows.md)
settles which before [C-441](../stories/C-441-context-management-doc.md) describes it.

## Approach

### C-441 — the context-management page

What a user needs to be able to answer: what fills the context, what flux does when it fills, what is
lost, what is kept, how to control it, and what it means for a session afterwards. Grounded in the
mechanism that exists, not in the mechanism we wish existed.

### C-442 — the peer-docs gap audit

Context management is *one* instance. The useful question is what **else** is obvious to everyone except
us. Audit against the harnesses users actually compare flux to — Codex, Claude Code, OpenCode, Pi — for
**topics they document that flux does not**, and classify each: *missing page* · *covered elsewhere and
unfindable* · *deliberately absent*.

⚠ **Verify against the live docs; do not write this from recollection.** A gap list assembled from
memory of what a competitor's docs "probably" contain is exactly the kind of confident-and-wrong
artifact this repo keeps catching. If a source cannot be checked, leave the row empty and say so.

**flux already has one evidence-backed comparison in-repo** —
`docs/reviews/single/2026-08-01-pi-flux-harness-comparison.md`, a nine-axis rubric from two isolated
source-level reviews. Its scores are a *starting hypothesis* for where docs lag: Flux rates **8.0 vs
Pi's 9.0 on Operator UX / customization** and **8.0 vs 9.0 on Embeddability / automation**, with the
reading that flux *"asks more of the embedder"* and *"exposes richer safety and workflow controls at
higher conceptual cost."* ⚠ That is a review of the **code**, not of the docs — but "higher conceptual
cost" is precisely the thing documentation is supposed to pay down, so it is where to look first.

### C-443 — does compaction ever actually fire?

The behaviour question that gates the page.

## Alternatives considered

- **Just write the context-management page.** Fastest, and it was the ask. ⚠ Rejected on its own: a page
  describing compaction as a working feature, when a 112k-event store contains zero instances of it,
  would be documentation of an intention. C-443 is a day at most and makes the page true.
- **A full docs audit from first principles.** Enumerate every concept flux has and check each is
  documented. Rejected as the starting point: it finds what we already know we have, and the gap here is
  the opposite — things users expect that we never thought to write.
- **Copy a competitor's table of contents.** Cheap and it would find real gaps. Rejected as the *method*
  because it also imports their structure, and flux's shape genuinely differs (authored flows, an
  approval envelope, replay). Compare topics, not tables of contents.

## Risks & open questions

- ⚠ **Documenting behaviour nobody has observed.** The failure mode this epic must not commit. If C-443
  finds compaction never fires, the honest page says what the threshold is and that reaching it is rare.
- ⚠ **A gap list written from memory.** See C-442.
- **Open:** whether "context management" is one page or a section. Token budgets, compaction, context
  packs, project context and session boundaries are related and currently scattered across three places
  that each look complete.
- **Open:** how much of this is a *findability* problem rather than a coverage one. `FLUX_COMPACT_CHARS`
  *is* documented — in a 500-line config table where nobody looking for a concept would find it.

## Acceptance / done

- A user can answer "what happens when my conversation gets long?" from the docs, and the answer matches
  what the code does.
- Whether compaction fires in practice is known, and the docs say so honestly either way.
- A checked, sourced list exists of topics peer harnesses document that flux does not, each classified —
  with the unverifiable rows left empty rather than guessed.
