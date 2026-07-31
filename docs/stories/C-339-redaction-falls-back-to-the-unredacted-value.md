---
id: C-339
title: "When redacted text stops parsing, `redact_and_hash_request` returns the *unredacted* value"
pillar: Core
status: ready
priority: 3
areas: [flux-sdk, flux-events]
note: "found by C-323's walker audit — crates/flux-sdk/src/test.rs:157 does `unwrap_or(canonical)`, so if text-level redaction corrupts the JSON badly enough that it no longer parses, the fallback hands back the ORIGINAL with the credential intact. The failure mode is silent and fails open"
---

# Redaction that fails to parse falls back to the unredacted value

## Goal

Make a redaction failure fail **closed**.

`crates/flux-sdk/src/test.rs:157` (`redact_and_hash_request`) redacts at the text level and then
re-parses. On a parse failure it does `unwrap_or(canonical)` — returning the **original,
unredacted** value. So the worse the redaction mangles the JSON, the more likely it is to hand back
the credential in full.

This is the exact corruption mode `parse_body` already documents, and C-323 measured a concrete way
to reach it: text-level substitution of a numeric credential can splice a quoted string into the
middle of a number (`216216` inside `1216216789`), leaving the document unparseable. Any such case
takes the fallback.

**The direction is what makes this a defect rather than a rough edge.** Every other redaction
decision in this tree is deliberately biased toward false *negatives* over false positives, but none
of them is biased toward *emitting the raw secret*. A redactor that cannot produce a safe value must
refuse, not shrug.

## Acceptance

- [ ] **Failing-first**: a request whose redaction produces unparseable output, shown returning the
      unredacted canonical value today. C-323's numeric-splice case is the cheapest route to one.
- [ ] The fallback fails closed. Decide what "closed" means here and say why — an error, an empty
      body, or a whole-value `[redacted]` are all defensible; silently returning the input is not.
- [ ] **Grep for the same shape elsewhere.** `unwrap_or`/`unwrap_or_else`/`unwrap_or_default` on the
      result of a redaction or sanitisation step is the pattern; list every hit with a verdict. This
      is the actual bug class and the point of the story is that the list ends up here rather than in
      an agent's context.
- [ ] ⚠ **`crates/flux-events/src/otel.rs`'s `redact_attr` passes numeric span attributes through
      unredacted.** Same class as C-323 but a different tree (typed OTel attributes, not
      `serde_json::Value`), so C-323 correctly left it alone. Close it here or file it onward with a
      reason — do not let it fall between the two stories.
- [ ] Full gate green in both workspaces.

## Notes

- Found by [C-323](C-323-redact-json-skips-numbers.md)'s walker audit, which fixed the four
  `serde_json::Value` walkers and flagged these two as out of its stated scope. That was the right
  call; this is the follow-through.
- Related principle: [C-315](C-315-secret-prefixes-misses-six-credential-shapes.md) chose mechanisms
  that fail toward false negatives *because* `Redactor` is the shared path for stream-json,
  cassettes, the approval sheet, evidence flush and harness ingest. That argument is about
  over-redaction; it does not license *under*-redaction on an error path.
- ⚠ `flux-sdk` is a **published** crate. Changing this function's failure behaviour may be a
  behavioural break; price it rather than assuming it is internal.
