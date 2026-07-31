---
id: C-303
title: "`http.request` has no structured query map, so a model-supplied value can inject request parameters"
pillar: Core
status: done
priority: 7
areas: [flux-web, flux-lang, security]
note: "security · authored Flux can only build a query by interpolating into a URL string, and nothing percent-encodes it — a value carrying & or = rewrites the request. flux-connectors C-30 is blocked on this and will not emit affected operations until it lands"
---

# `http.request` has no structured query map, so a model-supplied value can inject request parameters

## Goal

Give `http.request` a structured, percent-encoded `query` map, so authored Flux can build a query
string without interpolating untrusted text into a URL.

Today the only way to attach a query is to format it into the URL by hand, and **nothing encodes the
values**. A value containing `&`, `=` or `#` does not get escaped — it changes the request. This is
the exact sibling of the gap [L-101](L-101-form-urlencoded-body.md) closed for request *bodies*:
that story shipped `parse($record, as: "form")` because assembling `k={v}&k2={v2}` with `fmt`
interpolates values unencoded and "a value carrying `&` or `=` corrupts the body and can inject a
field." The same sentence is true of the query string, and it is still true today.

**Why this is more than theoretical.** flux-connectors emits composite operations that interpolate a
caller-supplied value straight into a URL, and its own source says so:
`crates/connector-flux/src/op.rs` — *"Query values are interpolated verbatim — nothing
percent-encodes them."* The connector Tool pack evaluates that emitted body, so a flux host that
registers connectors inherits it. The concrete case is
`zendesk-ticket-search`, whose URL is built as
`fmt("{base}/api/v2/search.json?query={query}")` over a **model-supplied** `query: String` — and
`zendesk.ticket.search` is one of the four operations `examples/zendesk.triage.flux` calls. So the
shortest path to a live Zendesk run currently ships an injectable operation.

That repo's **C-30** is filed as security and is explicitly blocked on this: its own fix is to
*refuse to emit* such operations until flux offers a structured query. Until this story lands, the
refusal is the only available mitigation there.

## Acceptance

- [x] `http.request` accepts a `query` map of scalars and percent-encodes each value per RFC 3986
      before appending it. The encoder is **shared with whatever already encodes**, not a fifth
      private copy — L-101's form encoder and the credential query placement in the connector pack
      are the existing precedents to look at.
      → `query_fields` + `append_query` (`crates/flux-web/src/http.rs`); the encoder is
      `flux_core::percent_encode_component` (`crates/flux-core/src/urlencode.rs`), which
      `flux-credentials`' `urlencode` and `flux-providers`' `percent_encode_segment` now also call.
- [x] **Failing-first test** `query_value_cannot_inject_additional_parameters`: a value containing
      `&injected=1` (and separately `#`, `=`, a space, and a non-ASCII character) arrives at the
      transport as one encoded parameter, not two. It fails today because no `query` map exists.
      → `crates/flux-web/src/http.rs`, `mod tests`.
- [x] **Decide and state the null/false rule.** A `null` field is omitted — that is how an unsupplied
      optional parameter means "do not send this"; but `false` and `0` are values and **must** be
      sent. L-101 settled exactly this for bodies; match it rather than inventing a second
      convention.
      → L-101's rule adopted verbatim, documented on `query_fields`;
      `query_omits_a_null_but_sends_false_and_zero`.
- [x] A duplicate key is an **error**, not a silent last-wins.
      → `append_query` refuses a key already present in `url`; `a_duplicate_query_key_is_an_error`.
      (The `query` record itself is a JSON object, so it structurally cannot carry two spellings of
      one key — the URL collision is the only way a duplicate can arise.)
- [x] Interaction with a URL that already carries a `?` is defined and tested — appending must not
      produce two `?` separators. The connector pack's `request.rs` already had to settle "who owns
      the `?`"; check what it decided and do not contradict it.
      → same rule as the connector pack's `auth.rs::place` (`if url.contains('?') { '&' } else
      { '?' }`); `query_appends_to_a_url_that_already_has_a_question_mark` and
      `append_query_owns_the_separator_and_keeps_the_fragment_last`.
- [x] `permission_subjects` and the `NetworkFetch` intent report the **encoded** URL, so an egress
      allow-list and the evidence log see what actually goes on the wire. ⚠ Do not put a
      query-placed **credential** into a subject — the connector pack deliberately reports the
      unauthenticated URL because `permission_subjects` cannot fail and so cannot consult a redactor.
      Preserve that property.
      → `reported_url`; `permission_subjects_and_the_intent_report_the_encoded_url` and
      `a_query_placed_credential_stays_out_of_the_subject_and_is_redacted`.
- [x] The op catalog is mirrored in **both** `crates/flux-flow/docs/ops-reference.md` and
      `website/docs/language/ops.md` (two different tests enforce these, per `AGENTS.md`).
- [x] Full gate green.

## Progress

Landed on `impl/C-303`. `http.request` gains one optional argument, `query`; nothing else about the
op changed, so this is additive.

**Which encoder, and why that one.** The story required sharing rather than a fifth copy, and the
tree had four hand-rolled encoders in two *different* dialects:

| where | dialect | space |
|---|---|---|
| `flux-lang` `urlencode_component` (L-101's form encoder) | WHATWG urlencoded | `+` |
| `flux-credentials` `urlencode` (OAuth authorize URL, query values) | RFC 3986 unreserved | `%20` |
| `flux-plugin` `percent_encode_component` (endpoint-template substitution) | RFC 3986 unreserved | `%20` |
| `flux-providers` `percent_encode_segment` (SigV4 canonical URI) | RFC 3986 unreserved | `%20` |

L-101's is deliberately **not** the one to reuse: its own doc comment records that `+` is correct
in `application/x-www-form-urlencoded`'s own name and wrong for RFC 3986, which is what this
Acceptance asks for. The other three are byte-identical to each other and to the connector pack's
`query_encode`, whose doc comment warns that "widening it from here would put two different
encoders on one URL" — so the RFC 3986 one is the shared encoder, and it now lives once, in
`flux_core::percent_encode_component` (L0, and every crate holding a copy already depends on
flux-core). `flux-credentials` and `flux-providers` were converted to call it. **`flux-plugin`'s
copy was left in place only because `crates/flux-plugin/src/host.rs` was fenced off for this run** —
converting it is a two-line follow-up and the natural place is whoever next touches that file.

**Three decisions a reviewer should challenge:**

1. **Duplicate key = a key already present in `url`.** A `query` record cannot contain a duplicate
   (it is a JSON object; serde collapses one). So the only reachable duplicate is a collision with
   the URL's existing query, and that is what `append_query` refuses.
2. **A fragment is split off and re-appended.** Not in the Acceptance, but appending `?a=1` to
   `https://h/p#frag` without it puts the query *inside* the fragment and sends nothing. Covered by
   `append_query_owns_the_separator_and_keeps_the_fragment_last`.
3. **`{"$secret": "ENV"}` is accepted in a query value**, through the same C-76 allowlist gate as a
   header. The alternative — refusing query credentials outright — would also satisfy the
   Acceptance's warning, but it would make `http.request` weaker than the plugin host's
   `AuthScheme::Query` path for no security gain. The property the story asks to preserve is kept by
   `reported_url`, which drops secret-valued parameters from the subject and the intent. Both the
   raw *and* the percent-encoded spelling of a resolved query secret are registered with the
   redactor — the redactor matches literally, so a token that only ever appears encoded in a quoted
   URL would otherwise survive.

`reported_url` falls back to the raw `url` when the `query` is malformed. That cannot under-report a
request: `execute` rejects the same input with a real diagnostic before any byte leaves the process.

## Notes

- **A paste-ready draft already exists** and should be read before designing this from scratch:
  `flux-connectors/docs/designs/query-encoding-flux-stories.md`, section F-1. It carries the RFC
  reference, the null/false rule, the duplicate-key decision, the shared-encoder requirement and this
  story's failing-first test name. Its sibling F-2 (optional composite-op parameters) is a real
  prerequisite on the **emitter** side, not here.
- Filed 2026-07-31 by the flux ↔ flux-connectors integration audit. It had **no story in either
  repository** despite being security-relevant and despite flux-connectors C-30 being blocked on it.
- Related: [C-304](C-304-http-request-returns-a-record.md) is the other missing `http.request` seam
  story the same audit found.
