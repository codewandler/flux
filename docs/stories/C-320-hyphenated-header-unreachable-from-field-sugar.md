---
id: C-320
title: "A hyphenated header name is unreachable from the `$resp.headers.x` sugar"
pillar: Language
status: done
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

- [x] **Decide the surface**, and it is a language decision rather than a web one: a quoted key in
      bracket access (`$x["a-b"]`), a quoted field segment, or something else. State what you chose
      and what you rejected. This changes flux-lang's grammar, so the cost is not only the parser.
- [x] **Failing-first**: a test showing the chosen spelling failing before the change.
- [x] Strict-access semantics are preserved — a missing quoted key must behave exactly like a missing
      plain field, including the `?` optional-access form and its error text.
- [x] ⚠ **The editor-tooling mirrors are manual and only one of four is guarded.** A grammar change
      must be propagated by hand to the website Prism grammar
      (`website/src/theme/prism-include-languages.js`), `codewandler/flux-tree-sitter` (Helix/Neovim/
      Zed, plus `.helix/languages.toml`), and the TextMate/IntelliJ grammars in
      `codewandler/flux-editors`. Only the Prism one has any mechanical check, and only for canonical
      header-option labels. Grep the other three; nothing else will tell you.
- [x] The node-kind and reference docs regenerate cleanly
      (`UPDATE=1 cargo test -p codewandler-flux-lang --test skill_in_sync` and `--test website_in_sync`),
      and changed semantics get hand-written prose — the generated tables do not cover that.
- [x] Full gate green in both workspaces. *(Landed on `main` in `47a26202`, which is an ancestor of
      `main` today; every `scripts/release-full-gate.sh` run since — including the ones that cut
      v0.59.1 and v0.59.2 — has carried this parser and passed.)*

## Progress

- Chosen surface: JSON-quoted bracket keys, for example `$resp.headers["content-type"]`. This reuses
  JSON string escaping, keeps dotted identifier sugar unchanged, and distinguishes quoted numeric
  object keys from unquoted numeric indexes. A new quoted-dot segment was rejected because it would
  introduce a second string syntax without improving the familiar map-access spelling.
- Failing-first parser, formatter, runtime, strict/optional-access, and malformed public-AST tests
  now cover the core behavior. Malformed bracket suffixes return ordinary diagnostics; they no
  longer reach an internal panic in formatting or evaluation.
- Editor mirrors are landed: `flux-editors` commit
  `e49ba2a332ed8215ec54d440ba537e4746218355`, and `flux-tree-sitter` commits
  `77e7ba61131a20a8f061e026727b634a7b8a5458` plus whitespace-alignment follow-up
  `7a90ffa6794972b3aa8dbf8a9b7b0755e3404f8b`. The Helix pin above now selects the follow-up.
- Focused core, generated-reference, mirror, corpus, query, Rust binding, and isolated Helix checks
  are green. The full Flux repository gate remains pending while unrelated active lanes share this
  worktree, so this story remains active.
- 2026-08-08 — closed. The pending gate resolved itself: the parser change reached `main` in
  `47a26202`, and every full gate since — including the two that cut v0.59.1 and v0.59.2 — has run
  against it. The last criterion was satisfied long before anyone ticked it. The status was left at
  `active`, which is not a value the board parses: `read_stories_with_warnings` drops such a file and
  `check` still exits 0, so this story was invisible to every board read for the whole interval.
  Silent-drop is a defect in its own right, and this file is the fixture for turning it into an error.

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
