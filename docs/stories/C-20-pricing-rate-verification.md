---
id: C-20
title: Verify the built-in pricing table against current vendor price sheets
pillar: Core
status: done
priority: 3
note: every row verified against vendor sheets 2026-07-02 — headline find gpt-5.5 was 4× LOW ($1.25/$10 → $5/$30, the live codex path); llama row adjusted; unverifiable rows marked ESTIMATED; headline rates pinned by test; source URLs + approximation notes (Bedrock cross-region premium, gpt-5.5 long-context tier) in the doc comment
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
- **Done (2026-07-02).** Every `builtin()` row checked against the vendor's current public sheet;
  all acceptance boxes hold:
  - **Fixed:** `gpt-5.5` was 4× LOW — shipped sharing gpt-5's launch rates ($1.25/$10) but bills
    $5/$30 (cache write $5, read $0.50). This is the row the live `codex` path resolves to, so
    every codex cost figure to date was understated ~4×. Also `meta-llama/llama-3.3-70b-instruct`
    ($0.12/$0.30 → $0.10/$0.32 per the current OpenRouter listing).
  - **Verified unchanged:** all Anthropic direct rows (Opus 4.8/4.7 $5/$25, Sonnet 4.6 $3/$15,
    Haiku 4.5 $1/$5 + exact 1.25×/0.1× cache tiers), Bedrock `anthropic.*` at direct list rates,
    `gpt-5` $1.25/$10, OpenRouter Sonnet $3/$15.
  - **Marked ESTIMATED inline:** `gpt-5-codex` (delisted; kept at last published price — never the
    live path) and the routed llama row (multi-provider, price floats).
  - **Approximations documented** in the doc comment: Bedrock cross-region (`us.`/`eu.`/`apac.`)
    ~10% premium over `global.` unmodelled (all prefixes at base rate); OpenAI gpt-5.5
    long-context premium (>272K input bills 2×/1.5×) unmodelled. Anthropic 1h cache-write tier
    (2×) not modelled — flux never requests it.
  - TODO hedge replaced with "Verified against the vendors' public pricing pages on 2026-07-02" +
    source URLs; `builtin_pins_vendor_verified_headline_rates` pins the headline rows
    (failing-first: pin written with correct gpt-5.5 values, observed fail, table fixed).

## Notes
- Requires web access for the vendor sheets; record source URLs in the table's doc comment.
- Subscription providers (`claude`/`codex`) are labelled equivalent-metered — their reference rates
  matter too.
