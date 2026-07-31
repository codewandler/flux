---
id: C-304
title: "`http.request` returns one flat string, so no caller can select a field from a response"
pillar: Core
status: done
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

- [x] `http.request` returns a record shaped `{ status, headers, body }` with a declared
      `output_schema`, so the analyzer can type a field access rather than failing at run time.
- [x] **Failing-first test**: a flow selecting a field from a JSON response body succeeds. It fails
      today because the response is one string.
- [x] **Decide how the body is carried, and say why.** `ToolResult.content` is a `String`
      (`crates/flux-runtime/src/lib.rs`), so this is either canonical JSON in `content` with a
      human-readable `view` — the precedent C-10 established — or a change to `ToolResult` itself.
      The second is wider than this story unless the first is shown not to work; state which you
      chose.
- [x] **A non-JSON or malformed body does not fail the call.** An HTML error page, an empty body or a
      truncated response must still produce a usable record with the status and headers intact —
      matching the stream-resilience posture that provider bytes never error a stream. A `404` is a
      *result*, not an error.
- [x] The human-facing rendering does not regress: whatever a person currently sees for a request in
      the evidence log and on the surface stays legible.
- [x] Secrets in **response** headers (`set-cookie`, and any header a caller registered) are still
      redacted; a structured header map must not become a new way for a token to reach a
      model-visible surface.
- [x] The op catalog is mirrored in **both** `crates/flux-flow/docs/ops-reference.md` and
      `website/docs/language/ops.md`.
- [x] Full gate green.

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

## Progress

**Landed on `impl/C-304`.** `http.request` now returns the record `{status, headers, body}`.

**How the body is carried — canonical JSON in `content`, human `view` beside it (the C-10
precedent).** `ToolResult` was NOT widened. The split already exists and already does exactly this
job: `flux_lang::runtime::execute_call` binds `content` into scope (so `$resp.body.data.id`
resolves) while the sink and the model are shown `view`. Keeping `view` byte-identical to the old
`HTTP <status>\n<headers>\n<body>` rendering is what makes the human-facing regression zero — the
model's experience of this op did not move at all. Widening `ToolResult` would have been a change
to an L2 contract every tool and every surface shares, to buy a property this seam already had.

**`body` is parsed only when it is a JSON object or array**, and is the raw text otherwise. That is
the interpreter's own rule (`jq_parse_input`), not a `content-type` sniff — plenty of APIs answer
JSON under `text/plain`. An HTML error page, an empty body, a truncated payload and a bare JSON
scalar all take the text arm, so the record always survives with its status and headers intact.

**Redaction moved into the op and runs AFTER the parse**, over decoded leaves and keys (response
header values are redacted as raw text). The dispatcher's redaction of `content` is no longer
sufficient on its own: by then a registered secret containing `"`, `\` or a newline is JSON-escaped,
so a literal match misses it — and the pattern redactor, whose token boundaries include `"`, can
rewrite JSON text into something that no longer parses. Proven by
`a_credential_echoed_back_in_a_response_header_or_body_is_still_redacted`, whose token carries both
a quote and a backslash on purpose.

**This is a BREAKING change to a published crate** (`codewandler-flux-web`): the string return is
replaced, not kept alongside the record. Pre-1.0 that prices a **minor** bump. The coordinator owns
the CHANGELOG/WHATS-NEW entries — this branch touches neither.

**Left for the caller of this seam:** a header name containing `-` (i.e. most of them) is not
reachable through the `$resp.headers.content-type` sugar — flux-lang field segments are
alphanumeric/underscore and `eval_jq_path`'s bracket index must be numeric. The working idiom is
`pick({items: $resp.headers, keys: ["content-type"]})`, and it is documented in the op's
`output_schema` and in both catalog files. Making the sugar reach a quoted key is a flux-lang
change, not a flux-web one.
