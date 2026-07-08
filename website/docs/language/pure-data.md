---
title: Pure data shaping
description: Computation without IO — fmt, field access, jq, expr, parse, and the obj/list value templates that replace shell-outs for data work.
---

# Pure data shaping

Pure nodes handle the data work inside a plan: formatting, field access, JSON extraction, arithmetic,
type coercion, and value assembly. They perform no IO, dispatch no operation, and never pause for
approval. Use them anywhere you would otherwise shell out just to reshape data.

In the text form, two of them have first-class spellings (`fmt(…)` and the `$var.path` field
access); the rest are written with the `@json` escape. All of them are ordinary nodes in the
JSON wire form.

## `fmt` — string interpolation

```flux
$label = fmt("BTC: {price} | 24h change: {change}%")
```

`fmt` substitutes `{name}` placeholders from already-bound symbols — the same syntax as
interpolation inside string literals, but explicit about being a formatting step. Unbound
placeholders are left verbatim (no silent data loss). The result is always a string.

`fmt(…)` is the **only** pure node with a call-style text spelling. Writing `expr(…)`,
`jq(…)`, or `peek(…)` call-style parses as an ordinary op call with that name — not the pure
node.

## Field access — `$var.path`

When the input is a bound symbol and the path is a simple dotted field path, extract fields
with the access sugar:

```flux
$raw   = web_fetch("https://api.coingecko.com/api/v3/simple/price?ids=bitcoin&vs_currencies=usd")
$price = $raw.bitcoin.usd
$kind  = $plan.kind
```

This lowers to a `jq` node. Notes:

- It is a **bind-value** form: valid on the right-hand side of `$x = …`, not inline as a call
  argument or `match` subject. Bind first, then use the symbol.
- `$obj.field` reads an object field; `$list.0` indexes into a list (`$list.0` is the first
  element).
- Access is **strict**: a missing field, an out-of-range index, or a field access on a non-object
  is a loud error — a typo fails fast instead of silently reading empty. A field that is *present
  but `null`* is never an error.
- Mark an access **optional** with a trailing `?` — `$x.field?`, `$list.0?` — to read `null`
  instead of erroring when a field may legitimately be absent.
- **Strictness is a reference-position rule.** A bare `$x.field` in a bind value, condition,
  value template, or `match`/`route` subject is strict (with the `?` opt-out). Field access
  *inside an `expr` computation* — `$x.field == y`, `$x.field + 1`, `filter`/`map` predicates —
  stays **lenient** (missing reads empty), since expr is for computed values where absent-as-empty
  is the established behaviour; a `?` there is accepted but redundant.

## `jq` — JSON path extraction

The full `jq` node supports dot paths with `[n]` array indexing, applied to the JSON content
of its input. Bracket paths and non-symbol inputs are written via `@json`:

```flux
@json {"kind": "bind", "name": "first", "value": {"kind": "jq", "path": ".results[0].value", "input": {"kind": "var", "name": "response"}}}
```

The path grammar is a strict subset of jq — dot-separated field names with optional `[n]`
suffixes (`.field`, `.field.nested`, `.field[0].nested`). No filters, pipes, or conditionals.
The extracted value keeps its natural JSON type. A model- or host-emitted `jq` node traverses
missing data leniently (an absent key or out-of-range index yields `null`); the native
`$x.field` sugar above is strict on missing data unless you add the `?` opt-out.

## `expr` — safe inline computation

`expr` evaluates a whitelisted formula over named variables — a tiny recursive-descent
evaluator, not `eval`, not a shell. It is written via `@json`:

```flux
@json {"kind": "bind", "name": "total", "value": {"kind": "expr", "formula": "price * qty", "vars": {"price": {"kind": "var", "name": "price"}, "qty": {"kind": "var", "name": "qty"}}}}
```

Supported, by precedence (lowest to highest):

| category | forms |
|---|---|
| boolean | `\|\|`, `&&`, unary `!`, `true` / `false` |
| comparison | `==` `!=` `<` `<=` `>` `>=` (numeric when both sides are numbers, else lexicographic) |
| arithmetic | `+` `-` `*` `/` — `+` concatenates when either side is non-numeric text |
| functions | `round(x, n)`, `abs(x)`, `min(a, b)`, `max(a, b)`, `len(s)`, `lower(s)`, `upper(s)`, `trim(s)`, `replace(s, from, to)`, `repeat(s, n)`, `reverse(s)`, `contains(s, sub)`, `concat(a, b, …)` |
| atoms | numbers, `'…'`/`"…"` string literals, variables, parentheses |

Every variable used in the formula must be declared in `vars` (mapping names to symbol or
literal nodes) — undeclared identifiers error. Comparisons yield a bool, so an `expr` is also a
valid `when`/`unless`/`until`/`assert` condition.

## `parse` — type coercion

Converts a string result (typically from `jq` or `fmt`) into a typed value. Written via
`@json`:

```flux
@json {"kind": "bind", "name": "price_num", "value": {"kind": "parse", "as": "f64", "value": {"kind": "jq", "path": ".price", "input": {"kind": "var", "name": "raw"}}}}
```

`as` is one of `f64`, `i64`, `bool`, `json`, `string`. Coercion failures error rather than
silently defaulting (`"abc"` to `f64` is an error); `bool` is true only for `"true"` or `"1"`.

## Value templates — `obj` and `list`

A record or list whose leaves are variables or expressions is a **template**, assembled at
evaluation time. `{ … }` / `[ … ]` in text is a template exactly when it is *not* plain JSON —
an unquoted key or a `$var`/expression leaf makes it one; pure JSON stays a literal:

```flux
$summary = { ok: true, count: $n, intent: $extract.intent, items: [$a, $b] }
return { status: "done", refs: $refs }
```

- Each field value is an expression: `$var`, `$v.path`, `fmt(…)`, a literal, or a nested
  template.
- Templates are **pure**: the analyzer rejects effectful leaves (op calls, control flow) so
  templates stay side-effect free. Bind a call result first, then reference the symbol.
- Keys are barewords when identifier-safe, otherwise JSON-quoted: `{ "a-b": $x }`.
- An all-literal template is just JSON, and parses as a literal.

This is what makes `return { … }` work: a flow assembles its structured result from computed
symbols without a single shell-out.

## Putting it together

The classic fetch-extract-compute-format chain, with no bash and no approval pauses — the
native spellings used where they exist, `@json` for `expr`:

```flux
flow btc-double
  $raw   = web_fetch("https://api.coingecko.com/api/v3/simple/price?ids=bitcoin&vs_currencies=usd")
  $price = $raw.bitcoin.usd
  @json {"kind": "bind", "name": "doubled", "value": {"kind": "expr", "formula": "price * 2", "vars": {"price": {"kind": "var", "name": "price"}}}}
  $label = fmt("BTC: {price} | Double: {doubled}")
  return { price: $price, doubled: $doubled, label: $label }
```

For selecting and budgeting *context* — the other big pure-node use case — see
[Context packs](./context-packs.md).

## Related docs

- [Context packs](./context-packs.md) — pure nodes for model-visible evidence.
- [Execution model](./execution-model.md) — symbol values, interpolation, and truthiness.
- [Flows & syntax](./flows-and-syntax.md) — call, bind, and interpolation syntax.
