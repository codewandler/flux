# Railflux — the ASCII dataflow projection

Railflux is a **terminal-first, 7-bit ASCII dataflow view of a `DraftAst`**. It is one of the
projections in the notation workbench epic (`docs/designs/flux-notation-workbench.md`): canonical
`.flux` is the authored surface, `render_pretty` is the execution-path tree, and Railflux answers a
different question — *what flows where*.

This document is the **output specification**. It is written for the deferred reader (L-100), which
will accept a formal subset of what is described here and reject everything else.

- Renderer: `crates/flux-lang/src/rail.rs`, exposed as
  `flux_lang::render::{render_rail, render_rail_styled, render_rail_spans}`.
- CLI: `fluxlang rail [FILE]` (reads stdin when `FILE` is omitted).
- Goldens: `crates/flux-lang/tests/railflux_golden.rs`.

## Status

**Output only, and total.** There is no reader, no alternate file extension, no content sniffing, no
AST change and no runtime behaviour. Every `Node` variant and every semantically relevant field is
projected — there is no "pretty enough" omission, and no truncation.

## The shape

```text
[flow triage (ticket: Ticket) -> Answer]
  ticket --> classify(.) --> kind
  [parallel]
    [branch] --> docs
      ticket --> search(query: .)
    [branch] --> hits
      ticket.title --> grep(pattern: .)
  [match kind]
    [case "bug"]
      [confirm "Open issue?" risk: "medium"]
        hits, ticket --> create_issue(hits, ticket) --> issue
        issue --> RETURN
    [default]
      docs --> RETURN
```

That is the projection of the epic's shared `triage` fixture, pinned byte-for-byte by the goldens.

## The grammar

There are exactly **two** line shapes, and the first non-space character decides which:

| First character | Shape | Contains `-->` |
|---|---|---|
| `[` | a **region** | never |
| anything else | a **rail** | always |

That single rule is the whole disambiguation story, and it is why the notation is worth reading
back later.

### Rails

```
rail    ::= [ sources SP ] "-->" SP stage [ SP "-->" SP sink ]
          | sources SP "-->" SP sink                    ; a pure move, no stage
sources ::= read ("," SP read)*
read    ::= symbol ( "." field | "[" index "]" )* [ "?" ]
stage   ::= op "(" [ args ] ")"                         ; a call
          | "[" expr "]"                                ; a pure computation
sink    ::= "RETURN"
          | [ "memo" SP ] symbol [ ":" SP type ] [ SP "!" effect ]
```

- **Sources** are the distinct symbol reads the stage consumes, in walk order. They are the dataflow
  edges — the reason the notation exists. The stage still spells its own arguments, so the two
  columns are a summary and its detail, not a division of the truth.
- A stage with no inputs opens the line with a bare `--> ` (`--> read("README.md")`).
- A value that is *itself* just a symbol read has no stage at all: `docs --> RETURN`.
- Stage tokens are always self-delimiting — an identifier followed by `(`, or a `[`. Sinks are bare
  names. So the columns never collide, whichever ones are present.

### Regions

```
region  ::= "[" label "]" [ SP "-->" SP sink ]
```

The region's body is the following lines indented **two further spaces**. Arms are regions in turn:
`[then]`, `[else]`, `[case …]`, `[default]`, `[branch]`, `[catch e]`, `[do]`, `[undo]`,
`[acquire]`, `[finally]`, `[step]`.

A region label puts the construct's **primary** field positionally and every other field as
`key: value` — the same "structural words stay visible, options carry labels" discipline the design
sets for canonical Flux headers:

```text
[repeat 3 until: done collect: all]
[retry 3 backoff: "linear" delay: 250ms]
[throttle "sends" max: 5 window: 1m]
[await "inbox" as: String when: needed] --> reply
```

An **absent** labelled option means the AST's field is `None` or empty. It never means the renderer
ran out of room.

### Expressions

Inside a stage or a region label, arguments and expressions use **canonical Flux expression syntax**
— named inputs `key: value`, punning, field access, `parse(x, as: "f64")`, `fmt("…")`,
`thing person name "John"` — so a reader can reuse the existing expression grammar instead of
growing a second one. A node with no inline spelling (a control-flow node in expression position)
falls back to the language's own raw-node escape, `@json {…}`, which carries the whole subtree.

The one Railflux-specific token is **`.`** — "the value on the incoming rail". It is substituted only
when the stage has exactly one source *and* the argument is exactly that source, which keeps it
unambiguous. A punned object key is a declaration, never replaced by `.`.

## Canonical ASCII

Output is strictly 7-bit ASCII. Structural glyphs are ASCII by construction, and every span of
embedded content (names, literals, messages, selectors, type labels) passes through one escape pass
that rewrites any non-ASCII scalar as a `\u` escape — UTF-16 surrogate pairs above the BMP, so an
escaped JSON literal stays valid JSON — and any control byte likewise, so no content can break the
line-per-statement structure.

## Determinism

`render_rail` is a pure function of the AST. Object fields iterate in `BTreeMap` order, lists in
declaration order, and nothing consults the environment, a clock, or a hash seed. Equal ASTs render
byte-identically.

## Styling

`render_rail_spans` returns `(text, Role)` spans — the same substrate `render_styled_spans` uses —
so a colour surface decorates Railflux through the existing `Palette` rather than a second styling
model. Concatenating a line's spans reproduces that line of `render_rail` exactly; every connector
(`[`, `]`, `-->`, the indent runs) carries `Role::Connector`.

## Deliberate deviations from the design sketch

`docs/designs/flux-notation-workbench.md` sketches Railflux illustratively. Three departures, each
made to keep the notation total, deterministic and readable back:

1. **Regions instead of aligned arm glyphs.** The sketch draws match arms as `+--` / `` `-- `` with
   padding that aligns arms into a column. Column alignment does not nest, and it makes every line's
   width a function of its siblings — a bad property for a golden and a worse one for a reader. Arms
   are `[case …]` regions with plain two-space indentation instead.
2. **`key: value`, not `key=value`, inside stages.** The sketch writes `search(query=.)`. Using the
   canonical Flux spelling `search(query: .)` means L-100 can reuse the existing expression grammar
   rather than introduce a second argument syntax for the same concept.
3. **Stages always carry their delimiters.** The sketch elides a lone argument (`classify` for
   `classify(ticket)`). Railflux writes `classify(.)`, because "a bare identifier is a sink, an
   identifier followed by `(` is a stage" is what makes a rail line readable without lookahead.

## Known limits

- A `fmt` template's `{name}` placeholders are symbol reads, but they are not lifted into the source
  column — the template is spelled in full in the stage instead. Lifting them would require the
  renderer to parse the interpolation grammar.
- Composite `op` declarations are operations, not flows; `fluxlang rail` projects a program module's
  top-level flows and journey flows only.
