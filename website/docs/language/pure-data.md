---
title: Pure data shaping
description: Computation without IO — fmt, field access, jq, expr, parse, and the obj/list value templates that replace shell-outs for data work.
---

# Pure data shaping

Pure nodes handle the data work inside a plan: formatting, field access, JSON extraction, arithmetic,
type coercion, and value assembly. They perform no IO, dispatch no operation, and never pause for
approval. Use them anywhere you would otherwise shell out just to reshape data.

In the text form, `fmt(…)`, `parse(…)`, `peek x`, field access including array indexes
(`value.items[0].name`), value templates, and invertible native expressions have first-class
spellings. The `@json` escape remains only for pathological shapes: non-invertible `expr` formulas
and `jq` over a non-symbol input. All of them are ordinary nodes in the JSON wire form.

## `fmt` — string interpolation

```flux
label = fmt("BTC: {price} | 24h change: {change}%")
```

`fmt` substitutes `{name}` placeholders from already-bound symbols — the same syntax as
interpolation inside string literals, but explicit about being a formatting step. Unbound
placeholders are left verbatim (no silent data loss). The result is always a string.

`fmt(…)` and `parse(…)` are the only pure nodes with special call-style text spellings. Writing
`expr(…)`, `jq(…)`, or `peek(…)` call-style parses as an ordinary op call with that name — `peek`'s
native form is the keyword spelling `peek x`, and native formulas are written directly where they
are invertible, for example `ok = $score >= 0.8`.

For a template longer than a line, give `fmt` a
[multi-line string](./flows-and-syntax.md#multi-line-strings) — the content is verbatim, so an
embedded JSON body needs no escaping:

```flux
flow render-payload -> String
  env = "staging"
  body = fmt("""{
  "environment": "{env}",
  "checks": ["build", "test"]
}""")
  return body
```

The JSON braces do not need doubling. A `{…}` run whose contents are not a bound symbol is emitted
verbatim, and the scanner resumes just after the brace it rejected — so the real `{env}` placeholder
later in the template still resolves.

For structured payloads, prefer building the value with an [`obj` template](#value-templates--obj-and-list)
and letting the op serialize it. Formatting JSON as a string works, but it gives up the type checking
that a template keeps.

## Field access — `var.path`

When the input is a bound symbol and the path is a simple dotted field path, extract fields
with the access sugar:

```flux
raw = web.fetch("https://api.coingecko.com/api/v3/simple/price?ids=bitcoin&vs_currencies=usd")
price = raw.bitcoin.usd
kind = plan.kind
```

This lowers to a `jq` node. Notes:

- It is a **bind-value** form: valid on the right-hand side of `x = …` and as a pure value-template
  leaf. It is not valid inline as a call argument, condition, or `match` subject. Bind first, then
  pass or test the symbol. In particular, branch on `status`, not `response.status`, after binding
  `status = response.status`.
- `obj.field` reads an object field; `list[0]` indexes into a list (`list[0]` is the first element).
- Access is **strict**: a missing field, an out-of-range index, or a field access on a non-object
  is a loud error — a typo fails fast instead of silently reading empty. A field that is *present
  but `null`* is never an error.
- Mark an access **optional** with a trailing `?` — `x.field?`, `list[0]?` — to read `null`
  instead of erroring when a field may legitimately be absent.
- **Strictness is a reference-position rule.** A bare `x.field` in a bind value or value template
  is strict (with the `?` opt-out). Field access *inside an `expr` computation* —
  `$x.field == $y`, `$x.field + 1`, `filter`/`map` predicates —
  stays **lenient** (missing reads empty), since expr is for computed values where absent-as-empty
  is the established behaviour; a `?` there is accepted but redundant.

## `jq` — JSON path extraction

The full `jq` node supports dot paths with numeric array indexes, applied to the JSON content of its
input. Native source uses brackets — `first = response.results[0].value` — while the lowered AST
stores that index as the dot segment `.results.0.value`. The formatter restores brackets. A
non-symbol input, or a hand-built AST whose path string itself uses brackets, needs `@json` to
preserve that exact AST shape:

```flux
first = @json { "kind": "jq", "path": ".results[0].value", "input": { "kind": "var", "name": "response" }, "optional": true }
```

The path grammar is a strict subset of jq — dot-separated field names and numeric index segments
(`.field`, `.field.nested`, `.field.0.nested`). No filters, pipes, or conditionals.
The extracted value keeps its natural JSON type. A model- or host-emitted `jq` node traverses
missing data leniently (an absent key or out-of-range index yields `null`); the native
`x.field` sugar above is strict on missing data unless you add the `?` opt-out.

## `expr` — safe inline computation

`expr` evaluates a whitelisted formula over named variables — a tiny recursive-descent
evaluator, not `eval`, not a shell. Invertible formulas in bind RHS and condition positions can be
written directly:

```flux
ok = $score >= 0.8
when $count > 3 && $state == "ready"
  return true
```

Arithmetic over bound symbols is invertible too:

```flux
total = $price * $qty
```

A formula that uses `expr`'s *function library* is not spellable natively — `round(price, 2)` in
text would parse as an op call named `round` — so function-bearing formulas are written via `@json`.
Here the formula-local name `price` maps to the bound symbol `unit_price`:

```flux
unit_price = 12.345
rounded = @json { "kind": "expr", "formula": "round(price, 2)", "vars": { "price": { "kind": "var", "name": "unit_price" } } }
```

Supported, by precedence (lowest to highest):

| category | forms |
|---|---|
| boolean | `\|\|`, `&&`, unary `!`, `true` / `false` |
| comparison | `==` `!=` `<` `<=` `>` `>=` (numeric when both sides are numbers, else lexicographic) |
| arithmetic | `+` `-` `*` `/` — `+` concatenates when either side is non-numeric text |
| functions | `round(x, n)`, `abs(x)`, `min`, `max`, `sum(xs)`, `any(xs)`, `all(xs)`, `has(xs, v)`, `len(x)`, `first(xs)`, `last(xs)`, `lower(s)`, `upper(s)`, `trim(s)`, `replace(s, from, to)`, `repeat(s, n)`, `reverse(s)`, `contains(s, sub)`, `concat(a, b, …)`, `join(xs, sep)`, `split(s, sep)` |
| atoms | numbers, `'…'`/`"…"` string literals, lists, objects, variables, dotted variable access, parentheses |

Every variable used in the formula must be declared in `vars` (mapping names to symbol or
literal nodes) — undeclared identifiers error. Comparisons yield a bool, so an `expr` is also a
valid `when`/`unless`/`until`/`assert` condition.

## Deterministic list transforms

Pure cognition ops cover the common data-shaping jobs that used to require shell snippets or
model-backed "return JSON" prompts. Predicate-bearing ops use a `where` string evaluated with
`it` bound to the current element; extra comparands go in `vars`.

Tier 1 handles projection and list mechanics:

```flux
authors = map(items: issues, path: "author.username")
hot = filter(items: issues, vars: { "min": 3 }, where: "it.state == 'opened' && it.score > min")
label_lists = map(items: issues, path: "labels")
labels = flatten(items: label_lists)
rest = skip(items: authors, n: 1)
csv = join(items: authors, sep: ",")
parts = split(s: csv, sep: ",", trim: true)
```

Tier 2 handles reductions and boolean emitters that compose directly in conditions:

```flux
total = sum(items: issues, path: "score")
by_state = count_by(items: issues, path: "state")
by_author = group_by(items: issues, path: "author.username")
when any(items: issues, where: "it.score > 8")
  notify()
```

Tier 3 trims objects and chooses fallbacks:

```flux
slim = pick(items: issues, keys: ["id", "title", "state"])
public = omit(items: issue, keys: ["raw_payload"])
merged = merge_obj(objects: [defaults, override])
owner = coalesce(default: "unassigned", values: [issue.assignee.username?, issue.author.username?])
fields = keys(item: merged)
vals = values(item: merged)
```

Tier 4 uses Rust regex for bounded, linear-time string classification and extraction:

```flux
has_error = regex_match(pattern: "ERROR", s: line)
version = regex_extract(group: 1, pattern: "v(\\d+\\.\\d+\\.\\d+)", s: body)
```

Use `map`/`filter` when the input is already structured JSON and the transformation is a field
projection or predicate. Use `each` when every item must dispatch real work such as `read`,
`web.fetch`, or `task`.

## `parse` — type coercion

Converts a literal, bound symbol, or object/list template into a typed value. Bind a computed `jq`
or `fmt` result first; `parse(…)` itself does not dispatch another computation. It is special-cased
in the expression grammar, so it lowers to the pure node rather than an op call:

```flux
price_text = raw.price
price_num = parse(price_text, as: "f64")
form_body = parse({ grant_type: "password", password: secret, username: user }, as: "form")
```

`as` is one of `f64`, `i64`, `bool`, `json`, `string`, or `form`. Coercion failures error rather
than silently defaulting (`"abc"` to `f64` is an error); `bool` is true only for `"true"` or `"1"`.
`json` can parse JSON text or serialize a structured value as canonical JSON. `form` serializes a
flat record as `application/x-www-form-urlencoded` text; it rejects nested values and omits fields
whose value is `null`.

## Value templates — `obj` and `list`

A record or list whose leaves are variables or expressions is a **template**, assembled at
evaluation time. `{ … }` / `[ … ]` in text is a template exactly when it is *not* plain JSON —
an unquoted key or a symbol/expression leaf makes it one; pure JSON stays a literal:

```flux
summary = { count: n, intent: extract.intent, items: [a, b], ok: true }
return { refs, status: "done" }
```

- Each field value may be a symbol, literal, `fmt`, `jq`/field access, `expr`, `parse`, or a nested
  object/list template.
- Templates are **pure**: the analyzer rejects effectful leaves (op calls, control flow) so
  templates stay side-effect free. Bind a call result first, then reference the symbol.
- Keys are barewords when identifier-safe, otherwise JSON-quoted: `{ "a-b": x }`.
- An all-literal template is just JSON, and parses as a literal.

This is what makes `return { … }` work: a flow assembles its structured result from computed
symbols without a single shell-out.

## Putting it together

The classic fetch-extract-compute-format chain, with no bash, no approval pauses, and no
escapes — every step has a native spelling:

```flux
flow btc-double
  raw = web.fetch("https://api.coingecko.com/api/v3/simple/price?ids=bitcoin&vs_currencies=usd")
  price = raw.bitcoin.usd
  doubled = $price * 2
  label = fmt("BTC: {price} | Double: {doubled}")
  return { doubled, label, price }
```

For selecting and budgeting *context* — the other big pure-node use case — see
[Context packs](./context-packs.md).

## Related docs

- [Context packs](./context-packs.md) — pure nodes for model-visible evidence.
- [Execution model](./execution-model.md) — symbol values, interpolation, and truthiness.
- [Flows & syntax](./flows-and-syntax.md) — call, bind, and interpolation syntax.
