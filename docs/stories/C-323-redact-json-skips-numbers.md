---
id: C-323
title: "`redact_json` skips `Value::Number`, and an all-digit credential has no recourse but registration"
pillar: Core
status: in-progress
priority: 5
areas: [flux-web, flux-secret]
note: "found by C-315 — an all-digit credential is outside every redaction heuristic by construction (no prefix marks it; the contextual rule requires a letter so `secret_ttl=3600` survives), so registration is its ONLY protection; any walker that narrows which nodes it visits is therefore a hole in add_secret's guarantee, not an optimization"
---

# `redact_json` skips `Value::Number`

## Goal

Close the gap between what `add_secret` promises and what the JSON walkers actually visit.

C-315 closed six credential shapes with four mechanisms, and in doing so established a boundary it
could not cross: **an all-digit credential is outside every heuristic by construction.** No prefix
can mark it. The contextual assignment rule deliberately requires a letter, because that is what
keeps `secret_ttl=3600` and every other numeric config value intact. Entropy scoring was rejected
for the whole story and would not help here anyway.

So for an all-digit credential, **registration is the only protection there is**. That changes what
a skipped node means. `redact_json` in `crates/flux-web/src/http.rs` does not descend into
`Value::Number`, which is a reasonable optimization if you assume a secret is always a string — and
an outright hole once registration is the sole recourse. A registered numeric secret echoed back
inside a JSON number reaches the model.

The invariant this story restores: **a registered secret is redacted wherever it appears**, with no
node kind exempted. `add_secret`'s guarantee is total or it is not a guarantee.

## Acceptance

- [x] **Failing-first**: register an all-digit secret, have a response carry it as a JSON *number*,
      and show it reaching a model-visible surface today.
- [x] `redact_json` visits every node kind. Decide what a redacted number becomes — it cannot stay a
      number and carry `[redacted]` — and say why the chosen representation is right. This is a real
      design question: changing a number to a string changes the shape of the record a caller selects
      from, which is exactly what C-304 made observable.
- [x] **Audit the other JSON walkers, which C-315 explicitly did not.** Grep every place the tree
      walks a `serde_json::Value` for redaction — evidence flush, stream-json, whatif cassettes, the
      approval sheet, harness ingest — and list each with the node kinds it visits. Any other walker
      that narrows by node kind is the same defect. Fix them together or say why one cannot be.
- [x] The anti-censorship posture holds: ordinary numeric values (ports, timeouts, counts, ids that
      are not secrets) must survive untouched. Only *registered* values are affected — this story
      adds no heuristic.
- [x] Full gate green in both workspaces.

## Notes

- Found and deliberately not fixed by [C-315](C-315-secret-prefixes-misses-six-credential-shapes.md),
  which pinned the boundary with `an_all_digit_credential_is_registration_only_and_registration_is_total`
  in flux-secret and an `ACCOUNT_SECRET_ID=…` entry in C-216's corpus `UNCAUGHT` list. The test names
  the invariant this story has to make true.
- ⚠ C-315 also had to widen `is_marked_synthetic` to accept a numeric marker (`216216216`), because
  an all-digit literal cannot carry an alphabetic one. That is a small loosening of C-216's
  synthetic-literal guard, which exists so the corpus cannot be satisfied with fake data. Worth
  re-reading when this story adds more numeric fixtures.
- Related: [C-304](C-304-http-request-returns-a-record.md) moved redaction into the op and after the
  parse, over decoded leaves and keys, precisely because the dispatcher's string-level redaction
  misses JSON-escaped values. This story is the same argument one level down: the walker has to
  reach the node before the redactor can act on it.

## Progress

Implemented on `impl/C-323`. Merge base `0df177c2` (main tip, post-0.43.0).

**Failing-first.** `http::tests::a_registered_numeric_credential_echoed_back_as_a_json_number_is_still_redacted`
(`crates/flux-web/src/http.rs`). At the base the record `content` read
`{"body":{"account_id":216216216216216218,…}}` — the credential verbatim in the canonical value
that is bound to a session symbol and spliced into `{{symbol}}` interpolations. The `view` was
already clean (it redacts the raw body text), so the hole was exactly the structured record C-304
introduced.

**Representation.** A redacted non-string scalar becomes `Value::String("[redacted]")`, and the node
is retyped **only when redaction actually fired** (compare the JSON literal before/after) — never by
switching on the node kind. A sentinel number was rejected as indistinguishable from real data;
`null` as ambiguous with a legitimately-null field. `"[redacted]"` is already the marker every other
redacted node in the record carries. The C-304 shape cost is therefore paid only by a node whose
value the caller could not have used anyway.

**Walker audit** (every `serde_json::Value` tree walk that applies a `Redactor`):

| walker | visited before | fixed? |
| --- | --- | --- |
| `flux-web/src/http.rs` `redact_json` | String, Array, Object keys+values | **yes** — `redact_scalar` |
| `flux-flow/src/engine.rs` `redact_json_strings` (evidence flush → durable event store) | String, Array, Object **values only** | **yes** — now `redact_json_in_place`, keys + scalars |
| `flux-flow/src/cassette.rs` `collect_json_redactions` (durable `input_view`) | String, Array, Object **values only** | **yes** — reuses `redact_json_in_place`; see below |
| `flux-orchestrate/src/lib.rs` `redact_spawn_json` (sub-agent live reporter) | String, Array, Object keys+values | **yes** — scalar arm |
| `flux-plugin-protocol/src/lib.rs` `redact_secret_fields` | replaces a *named* field's value whole, any kind | **not the same defect** — name-based masking, never consults a `Redactor`, does not narrow by node kind |

Not walkers (checked, no change owed): `flux-cli/src/stream_json.rs`, `flux-flow/src/loop_host.rs`
`approve_batch`, `flux-flow/src/staged.rs`, `flux-sdk/src/test.rs` all redact the **serialized text**,
which already reaches numbers and keys. The approval sheet (`flux-tui/src/toolview.rs`) renders input
verbatim by decision (C-195) and has no walker. Harness ingest
(`flux-capabilities/src/datasource/harness_history.rs`) redacts field-wise on extracted `&str` with
no tree recursion.

**Cassette has two paths now.** `redacted_input_view` kept its order-preserving textual rewrite for
the case it was written for — every redaction landing on a string leaf, whose encoded `"…"` token is
self-delimiting — because the view is capped and a truncated head is what a person reads. A tree
needing a *key* or a *non-string scalar* redacted is re-encoded from the scrubbed value instead:
textual substitution of a bare number literal is unsafe (replacing `216216` inside `1216216789`
splices a quoted string into a number and leaves `input_view` unparseable, and the TUI re-parses it).
`string_leaf_replacements` is the predicate that chooses.

**Not done, deliberately:** the same total-walk logic now exists in four places. Consolidating it
would mean either a new `pub` item on the published `codewandler-flux-secret` or a new dependency
edge from `flux-web` (which takes a redaction *closure* precisely to avoid one) — both outside this
story's fence. Filed as an ADJACENT finding for the coordinator.
