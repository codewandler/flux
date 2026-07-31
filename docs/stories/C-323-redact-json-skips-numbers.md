---
id: C-323
title: "`redact_json` skips `Value::Number`, and an all-digit credential has no recourse but registration"
pillar: Core
status: ready
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

- [ ] **Failing-first**: register an all-digit secret, have a response carry it as a JSON *number*,
      and show it reaching a model-visible surface today.
- [ ] `redact_json` visits every node kind. Decide what a redacted number becomes — it cannot stay a
      number and carry `[redacted]` — and say why the chosen representation is right. This is a real
      design question: changing a number to a string changes the shape of the record a caller selects
      from, which is exactly what C-304 made observable.
- [ ] **Audit the other JSON walkers, which C-315 explicitly did not.** Grep every place the tree
      walks a `serde_json::Value` for redaction — evidence flush, stream-json, whatif cassettes, the
      approval sheet, harness ingest — and list each with the node kinds it visits. Any other walker
      that narrows by node kind is the same defect. Fix them together or say why one cannot be.
- [ ] The anti-censorship posture holds: ordinary numeric values (ports, timeouts, counts, ids that
      are not secrets) must survive untouched. Only *registered* values are affected — this story
      adds no heuristic.
- [ ] Full gate green in both workspaces.

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
