---
id: C-20
title: Verify the built-in pricing table against current vendor price sheets
pillar: Core
status: ready
priority: 3
note: the builtin() table ships "plausible public list prices" with an explicit TODO-verify hedge — since C-05/C-06/C-15 the numbers feed real cost reporting, so verify each rate, fix drift, stamp a verified-on date
---

# Verify the built-in pricing table against current vendor price sheets

## Goal
`flux_core::pricing::PricingTable::builtin()` carries an explicit hedge (`// TODO verify rates` —
"plausible public list prices captured for the mechanism's sake"). Since C-05/C-06/C-15 these
numbers drive user-visible cost output (`flux usage`, turn-end cost annotations, the server
endpoint). Verify every row against the vendor's current public pricing page, correct drift, and
stamp the table with a verified-on date.

## Acceptance
- [ ] Every model row in `builtin()` checked against the vendor's current public price sheet
      (Anthropic, OpenAI, AWS Bedrock, Google, and any others present); discrepancies fixed.
- [ ] Cache-tier derivations re-checked per vendor (Anthropic ephemeral write ≈ 1.25×/read ≈ 0.1×;
      OpenAI cached-input discount) and documented where a vendor's actual sheet disagrees.
- [ ] The `TODO verify rates` hedge replaced by a `verified against vendor pricing pages on
      <date>` note; rows that could NOT be verified (no public sheet) are explicitly marked
      estimated instead of silently plausible.
- [ ] A unit test pins at least the headline rows (e.g. current Anthropic/OpenAI flagship rates) so
      accidental edits are caught; `~/.flux/pricing.toml` overlay behavior unchanged.
- [ ] Gate green: `cargo test --workspace`, clippy `-D warnings`, fmt, `cargo test -p flux-codegate`.

## Progress
- (not started — filed 2026-07-02 from the in-code TODO during the ready-queue curation.)

## Notes
- Requires web access for the vendor sheets; record source URLs in the table's doc comment.
- Subscription providers (`claude`/`codex`) are labelled equivalent-metered — their reference rates
  matter too.
