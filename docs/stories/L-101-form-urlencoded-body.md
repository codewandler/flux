---
id: L-101
title: Serialize a record as a form-encoded request body
pillar: Language
status: done
note: "authored Flux could build a JSON body and nothing else, so every OAuth2 token endpoint was unreachable"
---

# Serialize a record as a form-encoded request body

## Goal

Let authored Flux produce an `application/x-www-form-urlencoded` request body, so a flow can call the
APIs that parse only that — including every OAuth2 token endpoint, which is form-encoded **by
specification**.

## What was measured

`http.request` reads its `body` argument with `Value::as_str` and forwards the bytes verbatim
(`crates/flux-web/src/http.rs:183-186`, `egress.rs:83-85`); it never serializes a record and never
looks at `content-type`. So a body has to arrive as *text*, and the only node that turned a record into
text was `parse(x, as: "json")` — `coerce_parse`'s `json` arm (`crates/flux-lang/src/runtime.rs`).
Every other candidate was checked and does not exist:

- no `encode`/`stringify`/`serialize` node kind — the generated node-kind set in
  `crates/flux-lang/src/schema.rs` has 50 kinds and none of them serializes;
- no `expr` builtin that escapes anything: the whole set is `round abs min max len lower upper trim
  reverse contains replace repeat concat sum any all has join split first last`
  (`crates/flux-lang/src/expr.rs:804-828`);
- no registered op that percent-encodes. All three percent-encoders in the tree are **private Rust**
  (`flux-credentials/src/lib.rs:1435`, `flux-plugin/src/host.rs:1861`, `plugins/jira/.../mod.rs:857`)
  and none is exposed as a tool;
- `parse` could not be told to do it either — the analyzer restricted `as_type` to
  `f64`/`i64`/`bool`/`json`/`string`, so `as: "form"` failed *analysis*, not merely runtime.

The consequence reached beyond flux. flux-connectors (C-144) could declare a form encoding but had to
assemble the pairs with `fmt`, interpolating each value **unencoded** — so a value carrying `&` or `=`
corrupts the body and can inject a field, and its Stripe connector still ships write operations that
address everything in the path because a body was not an option at all.

## Acceptance

- [x] `parse($record, as: "form")` serializes a record as `application/x-www-form-urlencoded`, and the
      analyzer accepts the new target.
      → `analyze.rs`'s `VALID`; **failing-first test**
      `runtime::tests::parse_as_form_encodes_a_record_as_a_form_body`, which returned the JSON text
      `{"grant_type":"password",…}` before the change.
- [x] Values are escaped, including the two characters that made the `fmt` workaround unsound.
      → `urlencode_component`, the WHATWG urlencoded serializer;
      `parse_as_form_escapes_every_byte_a_form_body_reserves`.
- [x] The record spellings `as: "json"` accepts are accepted here too — a record value, or text holding
      a JSON object, which is how an op result arrives.
      → `parse_as_form_accepts_a_record_that_arrived_as_json_text`.
- [x] A `null` field is omitted rather than sent as `key=`.
      → `parse_as_form_omits_a_null_field`.
- [x] A nested field is **refused, not flattened**.
      → `parse_as_form_refuses_a_nested_field`.
- [x] Docs: the node-kind table, the `parse` reference section, the skill artifact and the website
      mirror all say so.
      → `ast.rs` doc comment plus `UPDATE=1 cargo test -p codewandler-flux-lang --test skill_in_sync`
      and `--test website_in_sync`.

## Progress

Landed, uncommitted, in the main worktree. Additive: `as_type` gains a value, no node kind and no
syntax changed, so none of the four editor-tooling grammars needs a keyword propagated.

Four behaviors are wire decisions and are documented as such in `form_urlencode`:

1. **Sorted key order.** `serde_json::Map` is a `BTreeMap` in this build, so one record always encodes
   to one body — a body that reordered between runs could not be signed or cached.
2. **A `null` field is omitted.** This is what makes an unsupplied optional parameter mean "do not send
   this field" instead of sending the literal text `null`, and it is why a caller does not need a
   `when` guard per optional field.
3. **A nested field is an error.** The format has no agreed convention — Stripe writes
   `metadata[key]`, PHP and Rails write `a[b]` and `a[b][]` — and a key a vendor does not recognize is
   accepted and *ignored*, answering `200`. That is the worst failure available, so this refuses.
4. **A space is `+`.** `application/x-www-form-urlencoded` specifies the WHATWG serializer, not
   RFC 3986; picking `%20` would be subtly wrong in the format's own name.

The encoder is hand-rolled rather than taking `percent-encoding` as a dependency: `flux-lang` is L0
and the serializer is a dozen lines with an exhaustive test, so widening the innermost crate's
dependency set was not worth it.

Diagnostics name the **shape** of an offending value, never the value: a form body routinely carries a
client secret, and `parse_as_form_refuses_a_value_that_is_not_a_record` asserts the value does not
reach the message.

## Notes

- **The downstream consumer is flux-connectors C-144**, which landed the declaration side — a closed
  `body_encoding` axis on an operation, `json` by default — and recorded the unencoded-value gap as
  intentional pending exactly this. Its emitter can now bind a record and encode it here instead of
  assembling `fmt` pairs, which also lets its optional body fields drop their `when` guards (rule 2
  above) and makes its Stripe connector able to carry a real request body. That is a follow-up in that
  repository, not here; it needs a released flux-lang, since it pins `codewandler-flux-lang` from
  crates.io.
- The **query**-string half of the same gap is still open: `http.request` takes a URL string, so a
  query value is interpolated unencoded too. `parse(…, as: "form")` does not fix that — a structured
  `query` argument on `http.request` is the shape that would, and it is described in
  flux-connectors' `docs/designs/query-encoding-flux-stories.md` §F-1.
