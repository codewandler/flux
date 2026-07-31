---
id: C-303
title: "`http.request` has no structured query map, so a model-supplied value can inject request parameters"
pillar: Core
status: in-progress
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

- [ ] `http.request` accepts a `query` map of scalars and percent-encodes each value per RFC 3986
      before appending it. The encoder is **shared with whatever already encodes**, not a fifth
      private copy — L-101's form encoder and the credential query placement in the connector pack
      are the existing precedents to look at.
- [ ] **Failing-first test** `query_value_cannot_inject_additional_parameters`: a value containing
      `&injected=1` (and separately `#`, `=`, a space, and a non-ASCII character) arrives at the
      transport as one encoded parameter, not two. It fails today because no `query` map exists.
- [ ] **Decide and state the null/false rule.** A `null` field is omitted — that is how an unsupplied
      optional parameter means "do not send this"; but `false` and `0` are values and **must** be
      sent. L-101 settled exactly this for bodies; match it rather than inventing a second
      convention.
- [ ] A duplicate key is an **error**, not a silent last-wins.
- [ ] Interaction with a URL that already carries a `?` is defined and tested — appending must not
      produce two `?` separators. The connector pack's `request.rs` already had to settle "who owns
      the `?`"; check what it decided and do not contradict it.
- [ ] `permission_subjects` and the `NetworkFetch` intent report the **encoded** URL, so an egress
      allow-list and the evidence log see what actually goes on the wire. ⚠ Do not put a
      query-placed **credential** into a subject — the connector pack deliberately reports the
      unauthenticated URL because `permission_subjects` cannot fail and so cannot consult a redactor.
      Preserve that property.
- [ ] The op catalog is mirrored in **both** `crates/flux-flow/docs/ops-reference.md` and
      `website/docs/language/ops.md` (two different tests enforce these, per `AGENTS.md`).
- [ ] Full gate green.

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
