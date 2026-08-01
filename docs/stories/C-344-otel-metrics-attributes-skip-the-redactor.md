---
id: C-344
title: "`build_metrics` takes no `Redactor`, so the OTel metrics half never redacts — the module header says it does"
pillar: Core
status: done
areas: [flux-events]
note: "found by C-339's OTel audit. build_trace(stream, events, redactor) redacts its free-text attributes; build_metrics(stream, events, pricing) has no redactor parameter at all, so the `model` attribute ships verbatim. The module header at otel.rs:21 claims every free-text value landing in a span/METRIC attribute is redacted. Closing it changes a published crate's public fn signature, which is why C-339 filed it rather than folding it in"
---

# The OTel metrics projection has no redactor

## Goal

Make the metrics half of the OTel exporter honour the redaction claim the module header already
makes for it — or, if metrics are genuinely meant to be exempt, say so where the claim is.

`crates/flux-events/src/otel.rs`:

- `pub fn build_trace(stream, events, redactor)` routes its free-text attributes through
  `redact_attr`.
- `pub fn build_metrics(stream, events, pricing)` **takes no `Redactor` at all**, and pushes
  `("model", AttrValue::Str(m.to_string()))` verbatim.

The module header claims otherwise, in terms that name metrics explicitly:

> Every free-text value that lands in a span/**metric** attribute is passed through the caller's
> `Redactor` first … plus a handful of provider/**model**/outcome strings that are redacted again
> here as defense in depth.

Probe, one registered all-digit secret, one session, both projections:

```
TRACE   contains secret: false
METRICS contains secret: true
METRICS json: …{"key":"model","value":{"stringValue":"216216789"}}…
```

Reproduction (re-run independently during C-339's rework, not taken on trust) — in `otel.rs`'s test
module, with `SECRET = "216216789"`:

1. `create_session(SECRET)` / `begin_turn(…, SECRET)` — the secret *is* the model id.
2. `record_call_usage(&session, turn_id, SECRET, Usage { input_tokens: 10, output_tokens: 5, .. })`
   so `projection::cost_summary` produces a row keyed by that model.
3. `end_turn`, `load_stream`, then a `Redactor` with `try_add_secret(SECRET)`.
4. `build_trace(&session, &events, &redactor)` vs
   `build_metrics(&session, &events, &PricingTable::builtin())`, each through
   `encode_trace_json` / `encode_metrics_json`, and search the bytes for `SECRET`.

`cargo test -p codewandler-flux-events --features otel` — note the `otel` feature is a `run` leg of
`scripts/check-feature-gated-tests.sh`, not something `cargo test --workspace` reaches.

## What this is, precisely

The metrics attribute set is `session.id`, `model`, `account`, `agent.id`, `tier` (a literal), and
`op.name`. Compared against the trace side:

| attribute | trace | metrics |
|---|---|---|
| `model` | redacted (`turn.model`, `call.model` via `redact_attr`) | **verbatim** |
| `session.id`, `account`, `agent.id` | verbatim (deliberate — the C-129 correlation keys) | verbatim |
| `op.name` | verbatim (structured identifier) | verbatim |
| `tier` | n/a | a literal (`input`/`output`/…) |

So the gap is exactly one attribute: **`model` is the one value the trace side scrubs and the
metrics side does not.**

**This is a defense-in-depth asymmetry, not a demonstrated live leak.** A model id is a vendor
label, and the probe above only reaches it by registering a secret that *is* the model id. C-129's
own Progress note calls the trace-side scrub "defense in depth, since the underlying strings are
typically already scrubbed at write time by the subsystem that recorded them". The defect is that
the invariant is *declared* and unmet — a reader auditing this module is told metrics are covered.

**It was an oversight, not a decision.** C-129's Progress note records the signature
`build_metrics(stream, events, pricing)` with no redactor, and its redaction bullet says "before it
becomes a **span** attribute" while the metrics bullet mentions redaction not at all. Nothing in the
record excludes metrics on purpose; the module header simply overclaims.

## Acceptance

- [x] **Failing-first**: the probe above as a test — a registered secret in the model id reaches a
      metrics attribute today while the trace projection is clean.
- [x] Either metrics attributes go through the redactor, or the module header stops claiming they
      do. Say which and why.
- [x] ⚠ **Price the API break before starting.** `crates/flux-events/src/lib.rs:33` is
      `pub mod otel;`, so `build_metrics` is public API of the **published**
      `codewandler-flux-events`. Adding a `&Redactor` parameter is a breaking change and obliges a
      version decision. Note that `scripts/check-crate-versions.sh` is structurally blind to
      workspace-versioned published crates, so a PASS is not evidence that nothing is owed. Do not
      resolve this by adding a second `build_metrics_redacted` beside the unredacted one — a
      parallel path leaves the leaking function published, and the repo's standing rule is atomic
      replacement over compat bridges.
- [x] Extend `no_exported_span_attribute_carries_a_registered_secret` (or add its sibling) to sweep
      the **metrics** projection, so the two halves cannot drift apart again. The existing guard
      asserts over `build_trace` output only, which is why nothing caught this.
- [x] Full gate green in both workspaces.

## Notes

- Found by [C-339](C-339-redaction-falls-back-to-the-unredacted-value.md)'s OTel audit. C-339 closed
  the *span* half (documented the numeric/structured-identifier verdict and pinned the free-form set
  with a guard) and deliberately did not fold this in: the fix is a public signature change on a
  published crate, which is a version decision, not a doc fix.
- C-339 also narrowed the scope claim on `redact_attr` from "the exporter's other attributes" to
  "the other **span** attributes" and corrected the module header, so the tree no longer asserts
  something untrue while this story is open. **That correction is the thing to undo here** — if
  metrics start redacting, the header should go back to covering both halves.

## Progress

**The fork, resolved: the code was wrong, not the header.** `build_metrics` now takes a `&Redactor`
and routes its `model` attribute through `redact_attr`, and the module header's "span **or metric**"
claim is restored as the whole truth about this module.

Why that way round, and what was rejected:

- **Documenting an exemption was the alternative, and it has no argument behind it.** To write "the
  metrics half is exempt" you must be able to say why the *same string* — a model id — is dangerous
  enough to scrub on a span and safe to ship verbatim on a metric point, when both leave the process
  in the same export, to the same collector, over the same connection. There is no such reason; the
  record (C-129's Progress note) shows an omission, not a decision. A documented exemption would also
  freeze a shape where the one free-form value each projection emits is treated differently by each,
  which is precisely the drift the guard now forbids.
- **A second `build_metrics_redacted` was rejected** (and the story forbids it): a parallel path
  leaves the unredacted function published and callable, which is the defect, not a fix for it.
- **Over-redaction was the cost to check, and it is nil.** Only `model` changes. `session.id`,
  `account`, `agent.id`, `op.name` and the literal `tier` stay verbatim — the same
  structured-identifier verdict C-339 recorded for the span side, for the same reason (they are the
  C-129 correlation keys). With no secret registered a plain `Redactor` is a pass-through, pinned by
  a new assertion in `metrics_report_tokens_spend_and_op_error_rates_with_session_and_agent_attributes`
  that the `model` attribute still reads `claude-sonnet-4-6`.

**The API break, priced.** `pub fn build_metrics(stream, events, pricing)` becomes
`pub fn build_metrics(stream, events, pricing, redactor: &Redactor)` — a breaking change to the
published `codewandler-flux-events` (`pub mod otel;`), behind the non-default `otel` feature. Every
call site in the tree is inside this module's own test suite (five), swept and updated; a full-tree
grep for `build_metrics` finds nothing else. Pre-1.0 SemVer as `AGENTS.md` states it makes this a
**minor** bump — the release coordinator owns `CHANGELOG.md` and the version, so the breaking note is
handed off rather than written here.

**Guard.** `no_exported_span_attribute_carries_a_registered_secret` is renamed to
`no_exported_span_or_metric_attribute_carries_a_registered_secret` and now sweeps both projections
from one fixture (the story's probe: the registered all-digit secret *is* the model id, and a
`record_call_usage` keyed by it is what puts the `model` attribute on the token/spend points). A
sibling, `neither_encoded_otlp_body_carries_a_registered_secret`, asserts at the wire level — the
secret must appear in neither `encode_trace_json` nor `encode_metrics_json` output bytes.
