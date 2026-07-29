---
id: C-198
title: "The syntax page says strings are single-line — triple-quoted strings have shipped since L-39"
pillar: Language
status: done
priority: 10
epic: website-truth-and-identity
design: docs/designs/website-truth-and-identity.md
note: 'flows-and-syntax.md:118 states a falsehood, not an omission; the triple-quote token has zero occurrences anywhere under website/docs/language/ while the lexer has supported it since L-39'
---

# The syntax page says strings are single-line — triple-quoted strings have shipped since L-39

## Goal
`website/docs/language/flows-and-syntax.md:118` tells authors *"Strings are single-line; embed
newlines with `\n` escapes."* Triple-quoted verbatim strings are implemented
(`crates/flux-lang/src/lexer.rs:179-186`, `crates/flux-lang/src/parse.rs:1482`), specified
(`crates/flux-lang/docs/syntax.md:316-352`) and shipped (`CHANGELOG.md:4277`). Grepping `"""`
across `website/docs/language/` returns **zero hits**. The recommended spelling for multi-KB
prompts and embedded JSON is documented as not existing — on the page that owns text syntax.

## Acceptance
- [x] The false claim is gone. The Literals table row now reads "double-quoted, or `\"\"\"…\"\"\"` to
      span lines", and the prose points at a new `### Multi-line strings` section covering the
      verbatim rule (no escape processing, no comment stripping, no dedent), where the form is
      valid, and the two documented limitations.
- [x] A worked example lands in `pure-data.md` — a multi-line `fmt` template with an embedded JSON
      body, which is the case the feature exists for.
- [x] The `memo` text spelling is documented on `flows-and-syntax.md`, in the `## Binds` section
      where it belongs, linking to `durability.md` for the semantics rather than duplicating them.
- [x] Both new examples are complete ` ```flux ` fences and are parsed by the existing
      `complete_flux_fences_parse_and_legacy_syntax_stays_out`.
- [x] Failing-first: new `syntax_page_documents_multiline_strings_and_the_examples_parse` in
      `crates/flux-cli/tests/website_contract.rs`.
- [x] `website/src/theme/prism-include-languages.js` mis-highlighted the form and was fixed.

## Progress
- **The syntax page.** Replaced the Literals row and the "Strings are single-line" sentence, and
  added `### Multi-line strings` after String interpolation: the verbatim rule, the fact that this
  is the one construct allowed to span physical lines (terminator found by scanning for the next
  `"""`, not by dedent tracking), where it is valid, and a note covering the two limitations from
  the spec — content cannot contain `"""` and cannot end in `"`, with `fluxlang format` falling back
  to the escaped spelling so round-tripping stays safe.
- **`memo`.** Not duplicated. `durability.md:23-33` already documents the semantics and the
  annotation forms properly; the gap was only that the page owning binds never mentioned the
  variant. Added a short paragraph plus one example in `## Binds`, linking across.
- **A wrong claim caught before it shipped.** The first draft of the `pure-data.md` example asserted
  that literal `{` braces need no doubling because "a `{` followed by anything else is emitted
  as-is". Checking `optimize.rs::collect_interp_reads_str` made that look actively dangerous — the
  scanner takes `{` to the *next* `}`, so `{` + JSON + `{env}` appeared to swallow the real
  placeholder into a bogus symbol name. Reading the runtime side settled it:
  `runtime.rs:3818-3821`'s unbound branch pushes the open brace and **re-scans from just after it**,
  so `{env}` is found on the following pass and does resolve. The example is correct; the
  explanation now states the recovery behaviour precisely instead of hand-waving, and points
  readers at `obj` templates for structured payloads, which keeps the type checking that
  string-formatted JSON gives up.
- **Failing-first, verified against HEAD rather than asserted.** The new test makes three claims;
  checked each against `git show HEAD:website/docs/language/flows-and-syntax.md` before landing the
  fix — `## Multi-line strings` occurred 0 times, `Strings are single-line;` occurred once, and
  `git grep '"""'` over `website/docs/language/*.md` at HEAD returned nothing. All three assertions
  would have failed. The test does not stop at "the page mentions it": it also requires at least one
  **complete** Flux example on the site to use a `"""` string and parse, so the documented form
  cannot drift away from the lexer.
- **The unguarded Prism mirror was in fact wrong** (`AGENTS.md:159` — grep it after syntax work,
  nothing else reports it). Its `string` pattern is `/"(?:\\.|[^"\\\r\n])*"/`, which excludes
  `\r\n` and therefore *cannot* match a block spanning lines; the example would have highlighted as
  an empty string followed by loose keywords. Added a `triple-quoted-string` token before it,
  `greedy` so the block reclaims any `#` that the non-greedy `comment` rule matched first (a `#`
  inside a `"""` block is content, per the spec). This is the same ordering Prism's own Python
  grammar uses for the identical problem. Verified in the built HTML, not by eye: the block
  tokenises as `triple-quoted-string string`, and `{diff}` inside it as
  `triple-quoted-string string interpolation variable`.
- Gate: `cargo test -p flux-cli --test website_contract` — 14 tests green, including the new one.
  `npm run build` clean.

## Notes
- Guarding the Prism grammar in general is out of scope — that question covers all four editor
  mirrors (`flux-tree-sitter`, `.helix/languages.toml`, `flux-editors`, the website), not just this
  one. Fix the highlighting here; file the guard separately if it turns out to be wrong.
- `crates/flux-lang/docs/syntax.md` is the specification and stays authoritative; the website page
  is a public restatement, not a second source of truth.
