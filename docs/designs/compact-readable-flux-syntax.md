# Design: compact, readable Flux source

**Story:** L-93 · **Status:** implemented

## Decision

Keep Flux's explicit control-flow words and indentation, but remove routine data-plumbing repetition.
All new syntax lowers to the existing AST. Old `$`/`do`/object-wrapper/raw-millisecond forms remain
accepted; the formatter emits the new source form.

```flux
flow route(utterance: String, caller_id: String) -> RouteResult
  extract = intent_extract(
    utterance,
    schema: schema,
    intents: intents,
  )
  intent = extract.intent?
  booking_cancel(context)
  return { intent, slots, response, escalated: false }
```

## Claims this proposal makes

These are hypotheses the implementation must prove, not aesthetic assertions:

1. **Most removable source weight is plumbing, not control flow.** In the existing 91-line
   `call-routing` sketch, `$` appears 39 times and `field: $field` appears 17 times. Removing those
   repetitions saves more source than abbreviating meaningful words such as `confirm`, `retry`, or
   `parallel`.
2. **One expression grammar is easier to read than statement-only exceptions.** A discarded result
   should look like the same call used on a bind RHS: `notify(message)`, not `do notify message`.
   `do` remains an accepted compatibility spelling, so stored source is not broken.
3. **Named inputs should look named.** The AST/runtime already define a multi-parameter call as one
   object map. `write(path: target, content: text)` projects that contract directly and removes the
   otherwise-semantic-free `{...}` wrapper.
4. **Punning is safe where identity is exact.** `{ result }` can only mean `{ result: result }`; it
   adds no inference and cannot change authorization, evaluation order, or runtime effects.
5. **Whitespace should expose structure, not prevent it.** Newlines and trailing commas inside a
   delimiter improve diff quality and scanning while lowering to the same AST as the one-line form.
6. **Token efficiency must not create a second language.** Human and compact formatters differ only
   in whitespace/string layout; both remain accepted Flux and exactly round-trip.

Claims 1 and 6 get mechanical size/round-trip checks. Claims 2–5 are pinned by old/new AST-equivalence
tests and corpus examples. Human readability itself is not reduced to a synthetic score.

## Before and after

### Symbols and result records

Current canonical source repeats both the sigil and each output key:

```flux
flow finish
  $intent = $extract.intent?
  $slots = $extract.slots?
  return { intent: $intent, slots: $slots, response: $response, escalated: false }
```

Proposed canonical source keeps the data names visible once:

```flux
flow finish
  intent = extract.intent?
  slots = extract.slots?
  return { intent, slots, response, escalated: false }
```

### Named operation inputs

Current source wraps the real input map in a positional argument container:

```flux
$result = eval_run({adapter: "terminal-bench", tasks: ["chess-best-move"], trials: 1, agent_timeout: 180})
```

Proposed source is multiline when that helps a reader and still compiles to one `Obj` argument:

```flux
result = eval_run(
  adapter: "terminal-bench",
  tasks: ["chess-best-move"],
  trials: 1,
  agent_timeout: 180,
)
```

When the field and symbol agree, the call may pun the input:

```flux
booking = booking_create(slots, caller: caller_id)
```

### Calls, indexes, and time

```flux
# accepted legacy source
do notify $items.0
timeout 60000 -> $answer
  do fetch $url

# proposed canonical source
notify(items[0])
timeout 1m -> answer
  fetch(url)
```

The words that carry runtime semantics remain intact. The removed tokens are syntax bookkeeping.

## Grammar rules

- A non-reserved bare identifier is a symbol reference. `$identifier` remains the unambiguous legacy
  and reserved-name escape. At statement start, `name [':' Type] '=' expr` is a bind.
- `name(...)` is an op call. A dotted/hyphenated op name is recognized only when followed by `(`;
  otherwise `name.field`/`name[0]` is value access.
- A generic call with one unlabeled expression keeps sole-parameter sugar. A call containing a label
  or multiple bare identifiers is a named input map: `x` puns to `x: x`; `x: expr` is explicit. A
  non-identifier positional expression may not be mixed with named entries. Duplicate names error.
  The resulting AST is `Call.args = [Obj{...}]`. `parse(value, as: "type")` remains intrinsic.
- Objects accept `name`/`$name` puns. Parentheses, braces, and brackets accept newline/comment trivia
  and trailing commas without participating in outer indentation.
- Access accepts `.field`, `[integer]`, and a trailing `?` on either hop; it lowers to the existing
  `Jq` path string.
- Duration-only header slots accept an integer (legacy milliseconds) or exact integer suffixes `ms`,
  `s`, and `m`; formatter output uses the shortest exact representation.
- Bare identifiers colliding with language keywords or literals are formatted with `$`. One shared
  reserved-word predicate is used by parsing and formatting.

## Compatibility and formatting

The public Rust/JSON types do not change. `format` changes textual output, which is a canonical-source
migration rather than an execution-format break. It uses two-space indentation and multiline strings;
`format_compact` uses one-space indentation and escaped strings. Both are strict-parser inputs and
must round-trip exactly, including degenerate/unspellable AST shapes via the existing legacy/`@json`
escapes.

The `.flux` extension has one meaning: Flux-Lang text. Hosts must not content-sniff a leading `{` as
a JSON `DraftAst`; JSON remains available through typed Rust/API values and explicit JSON tooling.
This keeps editor diagnostics, CLI execution, and repository corpus checks on the same parser.

The formatter uses `$name` when the corresponding bare spelling would be a keyword/literal, and it
retains a legacy positional spelling for arbitrary ASTs whose multiple arguments cannot be represented
as a named map without changing their shape. Compatibility therefore covers both authored historical
source and the stronger property-test universe of any serializable `DraftAst`.

No implicit return, decorator stack, comma-keyword control header, new node kind, or new interpolation
surface is added. The intentionally aspirational type-declaration example moves out of the executable
`.flux` corpus until that separate language feature exists.

## Verification

Tests cover CST losslessness and ranges, old/new AST equivalence, named-label preservation, delimiter
recovery, duplicate/mixed-input diagnostics, reserved-name escapes, duration overflow, indexed access,
and randomized canonical/compact round-trips. The root `examples/*.flux` directory is a release corpus:
every file must produce an error-free lossless CST and strict module, the LSP must publish no parse
diagnostics for it, and every embedded flow must survive both canonical and compact projections. A
representative source comparison records the byte
reduction as a tokenizer-independent proxy; provider tokenizers do not belong in the L0 crate.

Acceptance for the representative routing fixture is at least an 8% reduction in non-comment source
bytes versus the compatibility spelling. This is deliberately a proxy: providers tokenize `$name`,
punctuation, and identifiers differently, while bytes are deterministic and correlate with context
size without adding a tokenizer dependency to an L0 language crate.
