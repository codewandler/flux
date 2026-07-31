---
id: C-339
title: "When redacted text stops parsing, `redact_and_hash_request` returns the *unredacted* value"
pillar: Core
status: in-progress
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

- [x] **Failing-first**: a request whose redaction produces unparseable output, shown returning the
      unredacted canonical value today. C-323's numeric-splice case is the cheapest route to one.
      → `crates/flux-sdk/src/test.rs` `a_redaction_that_stops_parsing_never_returns_the_unredacted_request`.
      At the merge base it printed the record with `"account_id":1216216789` intact.
- [x] The fallback fails closed. Decide what "closed" means here and say why — an error, an empty
      body, or a whole-value `[redacted]` are all defensible; silently returning the input is not.
      → whole-value `[redacted]`; see [What "closed" means here](#what-closed-means-here).
- [x] **Grep for the same shape elsewhere.** `unwrap_or`/`unwrap_or_else`/`unwrap_or_default` on the
      result of a redaction or sanitisation step is the pattern; list every hit with a verdict. This
      is the actual bug class and the point of the story is that the list ends up here rather than in
      an agent's context. → [The census](#the-census).
- [x] ⚠ **`crates/flux-events/src/otel.rs`'s `redact_attr` passes numeric span attributes through
      unredacted.** Same class as C-323 but a different tree (typed OTel attributes, not
      `serde_json::Value`), so C-323 correctly left it alone. Close it here or file it onward with a
      reason — do not let it fall between the two stories.
      → closed here, as **not a hole**, with the reason recorded on `redact_attr` and pinned by a
      test; see [The OTel verdict](#the-otel-verdict).
- [x] Full gate green in both workspaces.

## What "closed" means here

`redact_and_hash_request` now collapses the **whole value** to `[redacted]` when the redacted text
no longer parses (`crates/flux-sdk/src/test.rs`, the `unwrap_or_else` at the end of the function).

Chosen because it keeps the fixture *replayable*: the hash is computed over the redacted text either
way, and `ServingProvider` recomputes exactly the same text for the same request, so a `check()`
re-drive still matches on the hash. The only thing lost is the record's human-inspectable `request`
field — and only for records whose remaining content would have been a credential.

Rejected:

- **`Err`** — loud and safe, but it destroys a live recording *after* the model call has been paid
  for, at a point the caller cannot act on (the credential is in the request they authored).
  `record()` would fail wholesale over one node.
- **An empty body** (`null`, `{}`) — indistinguishable from a request that genuinely had nothing
  there. That is the same ambiguity C-323 rejected sentinel numbers and `null` for on the response
  side.

**Not done here, deliberately:** the durable fix is to redact the parsed `serde_json::Value`
node-by-node so the document can never desynchronize at all. That needs the shared total-walk
[C-338](C-338-four-copies-of-the-total-walk.md) owns — adding a fifth hand-rolled walker is the
exact thing C-338 exists to stop. This story makes the boundary fail closed; C-338 removes the
boundary.

## The census

Every `unwrap_or`/`unwrap_or_else`/`unwrap_or_default` reachable on the result of a redaction or
sanitisation step, taken from the exhaustive list of `Redactor::redact` / `redact_secret_fields` /
`redact_plugin_echo` call sites in both workspaces (non-test code).

| site | shape | verdict |
|---|---|---|
| `crates/flux-sdk/src/test.rs` `redact_and_hash_request` | `from_str(&redacted_str).unwrap_or(canonical)` | **the defect.** Fixed by this story. |
| `crates/flux-flow/src/cassette.rs` `redacted_input_view` | `Err(_) => (redactor.redact(input_json), true)` on a re-encode failure | **safe.** The fallback is the whole-text scrub — strictly safer than the input, and the comment already says so. |
| `crates/flux-flow/src/loop_host.rs` (`action_batch.proposed`) | `.redact(&to_string(&batch).unwrap_or_default())` | **safe, opposite direction.** The `unwrap_or_default` is on the *input* serialization; a failure feeds the redactor `""`, so the observation loses data rather than leaking it. |
| `crates/flux-web/src/http.rs` `collect_headers` | `redact(value.to_str().unwrap_or("<binary>"))` | **safe.** The fallback is on the header's UTF-8 decode, *before* redaction; a non-UTF-8 header becomes `<binary>` and is then redacted. |
| `crates/flux-web/src/http.rs` `parse_body` | `_ => Value::String(redact(&body))` on a parse failure | **safe.** The unparseable branch still redacts. This is the shape `redact_and_hash_request` should have had. |
| `crates/flux-web/src/http.rs` `reported_url` | `append_query(raw, &public).unwrap_or_else(\|_\| raw.to_string())` | **safe.** `public` is the query with `$secret` params dropped; the credential lives in `params`, never in `raw`, so the fallback under-reports the *query* but cannot re-add a secret. Documented on the function. |
| `crates/flux-cli/src/plugin_cmd.rs` (echo render) | `to_string_pretty(&value).unwrap_or_else(\|_\| value.to_string())` | **safe.** `value` was already through `redact_plugin_echo`; both branches render the same redacted value. |
| `crates/flux-plugin-protocol/src/lib.rs` `redact_secret_fields` | — | **no fallback.** In-place walk, total over objects and arrays. |
| `crates/flux-cli/src/plugin_cmd.rs` `redact_plugin_echo` | — | **no fallback.** Thin manifest lookup over the above. |

No other `Redactor::redact` call site in either workspace has a fallback on its result: they are
assignments, `map` closures, or `?`-propagating (`redact_chunk` in the same flux-sdk module already
returns `Err` on a parse failure rather than falling back).

## The OTel verdict

`crates/flux-events/src/otel.rs`'s `redact_attr` takes a `&str` and returns `AttrValue::Str`, so a
numeric attribute never reaches it. Audited: **that is not the C-323 hole, and there is nothing to
close.**

C-323's defect was a walker over *arbitrary vendor JSON* that narrowed by node kind, so an all-digit
credential the vendor happened to send as a number escaped. Nothing in this exporter walks arbitrary
JSON. Every `AttrValue::Int` it emits is read as an `i64` from a *named* key of flux's own schema —
`turn.id`, `turn.iterations`, `plan.step`, `call.duration_us`, `call.retries`,
`call.oauth_refreshes`, `call.transport_fallbacks`, `call.ttft_us`, `call.input_tokens`,
`call.output_tokens`. Each is a counter, a duration or an internal id that flux computed; none is a
value a caller, a model or a vendor chose the *type* of. A number is a number here because the
schema says so — exactly the property C-323's walker could not rely on. Routing them through the
redactor would also cost the OTLP int type on every ordinary counter and break collector-side
aggregation, for no reachable gain.

Structured identifiers (`session.id`, `account`, `agent.id`, `op.name`, `call.stage`,
`turn.outcome`) also stay verbatim: they are operator- and flux-assigned labels that exist so a
collector can correlate a trace (C-129), they carry no vendor content, and over-redacting
identifiers is the failure mode C-315 chose its mechanisms to avoid.

What *is* reachable is the free-form set — the turn's model, a call's provider/model, and a failed
op's error text. All three were already redacted; they are now **pinned** by
`no_exported_span_attribute_carries_a_registered_secret`, which plants an all-digit registered
secret in every one of them and sweeps *every* attribute of *every* span in both spellings. Verified
to bite: stubbing `redact_attr` to pass its value through reds it on `turn.model`.

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

## Progress

Done (branch `impl/C-339`):

- `crates/flux-sdk/src/test.rs` — `redact_and_hash_request`'s re-parse fails closed to
  `[redacted]`; the rationale and the two rejected options are on the function. Two unit tests:
  the failing-first numeric-splice case, and its complement (an uncorrupted redaction still keeps
  the canonical object, so failing closed did not cost every ordinary record its shape).
- `crates/flux-events/src/otel.rs` — `redact_attr` now carries the audited verdict on what it does
  and does not cover, plus `no_exported_span_attribute_carries_a_registered_secret` to hold the
  free-form set.
- Census and OTel verdict recorded above.

**The behavioural change to price** (not done here — no crate version was touched): a
`ModelCallRecord` whose redaction corrupts the canonical JSON now stores `"request": "[redacted]"`
instead of an object. `hash`, `chunks`, and every non-corrupting record are byte-identical, so no
existing fixture changes and `check()`/`judge()` matching is unaffected. The only records that
change are ones that previously held a raw credential. `scripts/check-crate-versions.sh` passes, but
it is structurally blind to workspace-versioned published crates — a PASS is not evidence that
nothing is owed.

Follow-through owned elsewhere: [C-338](C-338-four-copies-of-the-total-walk.md) replaces the
text-level redaction here with the shared total-walk, which removes the failure path entirely.
