# Flux notation workbench — one AST, several readable projections

**Epic:** L-94 · **Status:** in progress

## Why

Flux has one honest execution model and one `DraftAst`, but different moments want different views:
canonical source for authors, a dataflow diagram for terminals, and compact regular forms for agents
and transport. Those views should meet at the AST instead of becoming alternate runtimes.

L-93 already made ordinary Flux source compact: bare locals, direct calls, named inputs, punning,
indexes, and exact durations are canonical. This epic completes that human surface and experiments
with projections whose value is stronger than saving another space.

## Decisions

1. **Railflux output comes first.** It is immediately useful as a deterministic 7-bit ASCII view of
   a `DraftAst`. Parsing waits until the drawing grammar has stabilized.
2. **Canonical `.flux` remains the authored language.** Alternate notations are selected explicitly;
   loaders never content-sniff them and the `.flux` extension never changes meaning.
3. **The AST is the boundary.** No notation changes analysis, authorization, runtime control flow,
   or effect dispatch. Conversion is syntax/projection only.
4. **Core plus escape beats a second giant grammar.** Glyph, Tape, and S-Flux spell common nodes
   natively and carry rare/pathological nodes through an explicit raw-AST escape.
5. **Ambiguity fails closed.** A future Railflux reader accepts only the formal subset it can map
   uniquely to `DraftAst`; visual resemblance is not enough.

## Canonical Flux completion

The target remains readable, word-oriented Flux:

```flux
flow triage(ticket: Ticket) -> Answer
  kind = classify(ticket)

  parallel
    branch docs
      search(query: ticket)
    branch hits
      grep(pattern: ticket.title)

  match kind
    case "bug"
      confirm "Open issue?", risk: medium
        issue = create_issue({ ticket, hits })
        return issue
    default
      return docs
```

The remaining inconsistency is parameterized control headers. Optional fields should use the same
comma-plus-label vocabulary as calls while structural words remain visible. Current space-keyword
forms stay accepted for compatibility.

## Railflux

Railflux is a terminal-first dataflow projection. Its canonical output is 7-bit ASCII even when a
surface later decorates the same role-tagged spans with colour.

```text
ticket --> classify --> kind

ticket ------+--> search(query=.) ------> docs
             |
ticket.title +--> grep(pattern=.) ------> hits

kind
 +-- bug --[? medium "Open issue?"]--> create_issue(ticket,hits) --> RETURN
 `-- * ---------------------------------------> docs --> RETURN
```

The first implementation is output-only and total: every node and semantically relevant field is
shown. Calls/binds/parallel/match/confirm/return get rails; constructs that do not fit a horizontal
dataflow shape use nested labelled regions. There is no lossy "pretty enough" omission. A later
reader targets the stable, unambiguous subset and rejects everything else.

## Compact notation candidates

### Flux Glyph

```text
F triage(ticket:Ticket)>Answer
= kind classify(ticket)
&
  +docs search(query=ticket)
  +hits grep(pattern=ticket.title)
?= kind
  |bug
    !?medium "Open issue?"
      = issue create_issue(ticket,hits)
      ^ issue
  |*
    ^ docs
```

The initial vocabulary is `F` flow, `=` bind, `^` return, `?` conditional, `?=` match, `?~` route,
`|` case, `|*` default, `&` parallel, `||` race, `??` fallback, `!?` confirm, `!!` assert, and `~=`
memo. `@{...}` is the raw-node escape.

### Flux Tape

Tape is flat and path-addressed. Every line identifies its AST location, so indentation is cosmetic
and diagnostics naturally name a transport address. The reader rejects duplicate paths, missing
parents, conflicting arm labels, and ambiguous ordering.

### S-Flux

S-Flux is a self-delimiting S-expression projection with named field keywords and `(ast {...})` as
its escape. It is data only: no macro system, evaluation, reader execution, or embedded Lisp runtime.

## Delivery order

1. L-95 Railflux renderer.
2. L-96 canonical named-option control headers (independent of L-95).
3. L-97 Flux Glyph.
4. L-98 Flux Tape.
5. L-99 S-Flux.
6. L-100 Railflux reader, only after L-95 output has stabilized.

All projections use the triage flow above as a shared golden fixture. Railflux snapshots must be
ASCII-only and deterministic; bidirectional compact notations must satisfy
`decode(encode(ast)) == ast`, including their escape form.
