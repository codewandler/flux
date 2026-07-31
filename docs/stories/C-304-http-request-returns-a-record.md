---
id: C-304
title: "`http.request` returns one flat string, so no caller can select a field from a response"
pillar: Core
status: ready
priority: 12
areas: [flux-web, flux-runtime]
note: "the connector pack's source claims this is 'a seam story on flux, filed rather than faked' — the audit found it was never filed. Blocks flux-connectors C-127 and every connector caller that wants .data.id"
---

# `http.request` returns one flat string, so no caller can select a field from a response

## Goal

Let `http.request` return a **structured** response — status, headers and a parsed body — so authored
Flux and a connector operation can select a field instead of receiving one opaque blob.

Today `crates/flux-web/src/http.rs` builds `content: format!("HTTP {status}\n{headers_text}\n{body}")`
and declares `output_schema: None`. Everything downstream gets that string. A flow that wants
`.data.id` gets nothing — and gets it **silently**, which is the worst version of this failure.

## Acceptance

- [ ] `http.request` returns a record shaped `{ status, headers, body }` with a declared
      `output_schema`, so the analyzer can type a field access rather than failing at run time.
- [ ] **Failing-first test**: a flow selecting a field from a JSON response body succeeds. It fails
      today because the response is one string.
- [ ] **Decide how the body is carried, and say why.** `ToolResult.content` is a `String`
      (`crates/flux-runtime/src/lib.rs`), so this is either canonical JSON in `content` with a
      human-readable `view` — the precedent C-10 established — or a change to `ToolResult` itself.
      The second is wider than this story unless the first is shown not to work; state which you
      chose.
- [ ] **A non-JSON or malformed body does not fail the call.** An HTML error page, an empty body or a
      truncated response must still produce a usable record with the status and headers intact —
      matching the stream-resilience posture that provider bytes never error a stream. A `404` is a
      *result*, not an error.
- [ ] The human-facing rendering does not regress: whatever a person currently sees for a request in
      the evidence log and on the surface stays legible.
- [ ] Secrets in **response** headers (`set-cookie`, and any header a caller registered) are still
      redacted; a structured header map must not become a new way for a token to reach a
      model-visible surface.
- [ ] The op catalog is mirrored in **both** `crates/flux-flow/docs/ops-reference.md` and
      `website/docs/language/ops.md`.
- [ ] Full gate green.

## Notes

- **Why this is filed now, and a correction to the record.** `flux-connectors`'
  `crates/connector-pack/src/tool.rs` documents the flat-string limitation and states *"That is a seam
  story on flux, filed rather than faked."* The 2026-07-31 integration audit greps all of flux's
  stories and found **no such story** — the only related hit is `D-98`, which *defined* the flat
  shape. The claim was false; this story makes it true.
- **What it unblocks:** flux-connectors **C-127** (separate the vendor response from the caller
  result) is correctly waiting rather than lying — the pack sets `output_schema: None` and returns
  `http.request`'s string whole, so it does not currently misrepresent its output. It cannot do better
  until this lands.
- ⚠ This changes a **published** crate's public behaviour (`codewandler-flux-web`). Weigh whether the
  record is additive alongside the existing string or a replacement, and price the version bump
  accordingly — flux uses the minor position as the breaking signal pre-1.0.
- Related: [C-303](C-303-http-request-structured-query.md) is the other missing `http.request` seam
  story from the same audit, and is the security-relevant one of the pair.
