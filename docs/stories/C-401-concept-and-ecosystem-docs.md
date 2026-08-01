---
id: C-401
title: "The shared vocabulary and the ecosystem page"
pillar: Core
status: done
design: docs/designs/ecosystem.md
note: "docs/concepts.md and docs/ecosystem.md become the authored source, mirrored into website/docs/ by website_in_sync rather than by a second sync mechanism"
---

# The shared vocabulary and the ecosystem page

## Goal

Give the three projects — flux, flux-connectors, flux-exchange — one written vocabulary and one
statement of who owns what, so the boundary between them stops being re-derived in each conversation.

## Acceptance

- [x] `docs/concepts.md` defines the terms the docs use, including the pair most often confused:
      `flux-runtime` decides whether something may happen, `flux-system` is where it happens — peers
      at L2, not stacked.
- [x] `docs/ecosystem.md` states the boundary test (one interrogative per domain) and distinguishes
      what ships from what is proposed.
- [x] `docs/designs/ecosystem.md` records the reasoning, including what the flux-connectors charter
      gets wrong and the runtime axis that replaces the plugin/connector dichotomy.
- [x] Both pages are mirrored into `website/docs/` by
      `crates/flux-lang/tests/website_in_sync.rs` — the existing golden mechanism, not a second one.
      Drift fails; a regenerating run writes and then goes **RED** (C-326).
- [x] `cargo test -p flux-cli --test website_contract` is green: the Flux fence parses and is a
      formatter fixed point, and the Concepts symbol text keeps the canonical bare spelling the
      suite pins.

## Progress
- Done. Landed with the epic C-394 and its stories, which the ecosystem design produced.

## Notes
- **Reviewed before landing** — `docs/reviews/single/2026-08-01-concept-docs-review.md` found ten
  issues in the first draft, including three red `website_contract` tests. Worth reading as a record
  of what a documentation change can break: the pages that *define* the vocabulary had taught
  brace-and-equals syntax the parser rejects, listed a capability (`workspace.write`) as an effect,
  and used connector-channel fields (`mode`, `exchange`) that do not exist — and because
  `ConnectorSettings` is deliberately not `deny_unknown_fields`, the fictitious ones would have been
  silently ignored rather than rejected.
- The first draft also shipped a `scripts/sync-website-docs.sh` that duplicated `website_in_sync.rs`
  and violated C-326 by writing and reporting green. It was deleted rather than fixed: the existing
  mechanism already had the right semantics.
