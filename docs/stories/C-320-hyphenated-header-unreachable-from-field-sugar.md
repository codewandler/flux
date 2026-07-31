---
id: C-320
title: "A hyphenated header name is unreachable from the `$resp.headers.x` sugar"
pillar: Language
status: ready
priority: 15
areas: [flux-lang]
note: "found by C-304 — `http.request` now returns a headers map, but `.headers.content-type` and `.headers[\"content-type\"]` both fail: field segments are alphanumeric/underscore and eval_jq_path's bracket index must be numeric, so the commonest header names need a pick() workaround"
---

# A hyphenated header name is unreachable from the field sugar

## Goal

C-304 made `http.request` return `{status, headers, body}`. The headers map is the point of the
story — and the header names callers most want are `content-type`, `x-request-id`, `retry-after`,
`www-authenticate`. None of them can be read with the natural spelling.

`$resp.headers.content-type` fails because flux-lang field segments are alphanumeric/underscore, so
the hyphen ends the segment. `$resp.headers["content-type"]` fails because `eval_jq_path`'s bracket
index must be **numeric**. Both spellings a user would try are errors.

The working idiom is `pick({items: $resp.headers, keys: ["content-type"]})`, which C-304 documented
in the response schema and both catalog files. That is an honest workaround, not an ergonomic
answer — and it is the first thing a caller hits after adopting the new record.

## Acceptance

- [ ] **Decide the surface**, and it is a language decision rather than a web one: a quoted key in
      bracket access (`$x["a-b"]`), a quoted field segment, or something else. State what you chose
      and what you rejected. This changes flux-lang's grammar, so the cost is not only the parser.
- [ ] **Failing-first**: a test showing the chosen spelling failing before the change.
- [ ] Strict-access semantics are preserved — a missing quoted key must behave exactly like a missing
      plain field, including the `?` optional-access form and its error text.
- [ ] ⚠ **The editor-tooling mirrors are manual and only one of four is guarded.** A grammar change
      must be propagated by hand to the website Prism grammar
      (`website/src/theme/prism-include-languages.js`), `codewandler/flux-tree-sitter` (Helix/Neovim/
      Zed, plus `.helix/languages.toml`), and the TextMate/IntelliJ grammars in
      `codewandler/flux-editors`. Only the Prism one has any mechanical check, and only for canonical
      header-option labels. Grep the other three; nothing else will tell you.
- [ ] The node-kind and reference docs regenerate cleanly
      (`UPDATE=1 cargo test -p codewandler-flux-lang --test skill_in_sync` and `--test website_in_sync`),
      and changed semantics get hand-written prose — the generated tables do not cover that.
- [ ] Full gate green in both workspaces.

## Notes

- Found by [C-304](C-304-http-request-returns-a-record.md), which shipped the `pick()` workaround and
  documented it rather than changing the language mid-story. That was the right call for that story
  and is why this one exists.
- Related and worth reading first: [C-301](C-301-tree-sitter-duration-suffixes.md) is the standing
  example of what happens when a language-surface change does not reach the editor mirrors — canonical
  Flux showing `ERROR` nodes in three editors, invisible for a release because that repo's corpus only
  used the old spelling.
- Adjacent, deliberately not folded in: `ToolSpec.output_schema` never types a field access —
  `OpSignature::from_spec` collapses an object schema to `TypeRef::Any` and `infer_type` returns `Any`
  for `Node::Jq`. So C-304's schema is honest documentation and a machine-readable contract, but the
  access is checked at run time. Closing that is an analyzer story, not this one.
