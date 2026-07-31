# Flux Glyph — the compact opcode notation

Flux Glyph is a **compact, indented, agent-facing projection of a `DraftAst`** (L-97). It is one of
the notations in the workbench epic (`docs/designs/flux-notation-workbench.md`): canonical `.flux`
is the authored surface, `render_pretty` is the execution-path tree, Railflux answers *what flows
where*, and Glyph answers *how do I write this down in as few tokens as possible without losing
anything*.

Unlike Railflux, Glyph is **bidirectional**: `parse_glyph(&format_glyph(&ast)) == ast` for every
`DraftAst` body.

- Module: `crates/flux-lang/src/glyph.rs`, exposed as `flux_lang::glyph::{format_glyph, parse_glyph, OPCODES}`.
- CLI: `fluxlang glyph [FILE]` (canonical Flux → Glyph), `fluxlang unglyph [FILE]` (Glyph → canonical Flux).
- Tests: `crates/flux-lang/tests/glyph_notation.rs`.

## Status

**Bidirectional, and total.** There is no new node kind, no new `.flux` syntax, no alternate file
extension, no content sniffing, and no runtime behaviour. Glyph is reached only by naming it: the
`.flux` parser rejects a Glyph document, and `parse_glyph` rejects canonical Flux source.

## The shape

```text
F triage(ticket:Ticket)>Answer
= kind classify(ticket)
&
  | docs
    search(query: ticket)
  | hits
    grep(pattern: ticket.title)
?= kind
  | "bug"
    !? "Open issue?" medium
      = issue create_issue(hits, ticket)
      ^ issue
  |*
    ^ docs
```

That is the epic's shared `triage` fixture, pinned byte-for-byte by the tests, and it parses to
exactly the AST the canonical spelling of the same flow parses to.

## Lines

A document is a sequence of lines. Blank lines and `#` comment lines are ignored. Every other line
is:

```
<indent><opcode>[ <operand>]
```

- **Indentation is exactly two spaces per level**, and a body indents exactly one level below its
  construct. An odd indent, a tab, or a skipped level is an error naming the line.
- The **opcode** is the first whitespace-delimited token. It is always separated from its operand by
  a space — there is no glued form.
- The **operand** is everything after it, and for every construct whose operand is an *expression*
  it is canonical Flux expression syntax (named inputs `key: value`, punning, field access, literals).
  Glyph does not grow a second expression grammar.

## The vocabulary

| Opcode | Construct | Operand |
|---|---|---|
| `F` | flow header | `[name][(param:Type, …)][>Return]` |
| `=` | bind | `name[:Type] <expr>` |
| `~=` | memo | `name[:Type] <expr>` |
| `^` | return | `<expr>` |
| `?` | conditional (`when`) | `<cond>`; body follows, `|*` carries the else |
| `?=` | match | `<subject>`; `\|` and `\|*` arms follow |
| `?~` | route | `<selector>`; `\|` and `\|*` arms follow |
| `&` | parallel | — ; `\|` branch arms follow |
| `\|\|` | race | `<timeout>[ > bind]`; `\|` branch arms follow |
| `??` | fallback | `[> bind]`; `\|` branch arms follow |
| `\|` | arm | a case value, a branch name, or nothing (fallback) |
| `\|*` | default arm | — |
| `!?` | confirm | `"<message>"[ <risk>]` |
| `!!` | assert | `<cond>[, "<message>"]` |

`|` is the **one** arm opcode; what it means is decided by the construct enclosing it, never by
guessing:

| Enclosing | `\|` operand | `\|*` |
|---|---|---|
| `?=` match | the case value, a canonical expression | the default body |
| `?~` route | the case label, a quoted string | the default body |
| `&` parallel | the branch name (must be distinct) | rejected — parallel has no default |
| `\|\|` race | the branch name (must be distinct) | rejected |
| `??` fallback | nothing — branches are tried in order | rejected |
| `?` conditional | rejected — a conditional has no labelled case | the else body |
| anywhere else | rejected — an arm needs an enclosing construct | rejected |

## Core plus escape

Glyph spells the fourteen constructs above natively. Two shapes carry everything else — the epic's
"core plus escape" decision, so the notation stays small instead of growing a second full grammar:

1. **Pass-through leaf.** Any statement whose canonical Flux spelling is a single line is written in
   canonical Flux, verbatim: `read("f")`, `pack += a`, `checkpoint "x"`, `each x in xs`, `p.q`. It is
   always a **leaf** — Glyph owns block structure through its opcodes, so a pass-through line that
   carries an indented body is an error.
2. **The escape `@{…}`.** The node's compact wire JSON on one line. Everything with no native
   spelling takes it: a multi-line canonical construct (`try`, `saga`, `scope`, a non-empty `each`),
   a node whose names or labels are unspellable, a bind or memo carrying an `@effect(…)` marker, a
   `parallel`/`race` whose branch names collide, and any statement whose canonical one-liner would be
   *misread* as an opcode (a read of a symbol literally named `F`).

The writer and the reader share one classifier, so a line is emitted verbatim only when the reader
would classify it as a pass-through. That is what makes the round-trip total rather than
best-effort.

## Diagnostics

Structure is Glyph's own, so structural errors are raised by the reader and name the Glyph line:

| Failure | Example |
|---|---|
| indentation not a multiple of two, a tab, or a skipped level | `line 3: Glyph indents two spaces per level; this line starts with 3` |
| a sigil-shaped token that is not an opcode | `line 3: unknown opcode `?!`` |
| an arm outside an arm-taking construct | `line 2: `\|` is an arm: it needs an enclosing …` |
| an arm kind the construct does not take | `line 3: `&` has no default arm` |
| a duplicate branch name, or a duplicate/misplaced default | `line 5: duplicate branch name `docs`` |
| a malformed `@{…}` escape | `line 2: a malformed `@{…}` escape: unknown variant `nope`` |
| a body under a leaf statement | `line 3: `read("a")` is a leaf statement and takes no body` |

Expressions are canonical Flux, so their diagnostics are the canonical parser's. The reader expands
the document to canonical Flux while keeping a line map, then rewrites the canonical `line N:` prefix
back to the Glyph line the author can actually see — blank and comment lines make the two numbers
diverge, and the Glyph one always wins.

Nothing is repaired. Every one of the failures above is refused with a location.

## The flow-header exception

Like `format`, the `F` header has no escape: an unspellable flow name, parameter name, or header
type is emitted verbatim and produces a loud error on the way back rather than silent corruption.
The analyzer rejects such names before they can reach a formatted artifact.
