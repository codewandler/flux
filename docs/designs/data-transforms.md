# flux-lang data transforms (epic)

**Status:** proposed 2026-07-08 · **Pillar:** Language · **Epic slug:** `data-transforms`

Ships the missing data-shaping surface — map, filter-with-predicate, reduce-shaped
aggregations, flatten, slicing, string join/split, object pick/omit, regex — as pure ops
grounded in one shared expression mini-language (the existing `Expr` engine, extended and
extracted). Plus one high-value parser story so native text can write `when $count > 3`
instead of the `@json` escape.

## Context

Flux-Lang deliberately keeps its node catalog small (43 kinds) and pushes composition into
**pure ops** wherever the runtime does not need to *understand* the operation
([flux-lang-evolution.md §5.1](flux-lang-evolution.md)). That decision left a documented
future-work slot: *"the collection transforms `map`/`filter`/`reduce`/`sort`/`dedupe`/`top`
(these overlap the pure cognition verbs)"*. `sort`/`dedupe`/`top`/`merge`/`compare`/
`len`/`first`/`last` shipped in `flux-tools/src/cognition.rs`. `filter` shipped with a
minimal `by`/`equals` predicate. **`map` and `reduce` were never shipped.**

Real-world plans are working around the gap in three ways, all measurable in-repo:

1. **LLM-as-transformer.** `ai.extract` is prompted "Return ONLY a JSON array"
   (`examples/advanced-code-review.flux:58-65`); `task` prompts include "Return ONLY the
   JSON array of tasks" (`examples/improve-tbench.flux:63,123`); `ai.reason` is used as a
   classifier to emit one word for a `route` selector. Every one of these is a
   non-deterministic, billable substitute for a deterministic transform.
2. **Bespoke Rust boolean-emitters.** `candidates_empty`, `score_compare`, `grade` return
   the string `"true"`/`"false"` **solely** so a `when`/`until` guard can branch on them
   (`crates/flux-eval/src/aggregate.rs:259`, `src/ops.rs:557-671`). The root cause is
   structural: `analyze.rs:1004-1014` restricts condition node kinds to
   `call | lit | var | expr`, and `expr` has **no native-text spelling** — so every
   predicate that isn't a bare call must be materialized by an op.
3. **Manual field-plucking loops.** Because `filter`/`sort`/`dedupe` accept only a single
   top-level field, and no `map` op exists, plans use sequential `each` loops or
   monolithic Rust ops (`improvements_aggregate`, `eval_scalar`, `candidates_advance`) to
   accomplish per-element projection.

Plugin payloads make this concrete. The gitlab plugin returns raw API arrays
(`issue_list`, `mr_list`, `pipeline_list`, ...); the natural queries — *"open MR titles"*,
*"issues where labels contains X"*, *"count of failed pipelines"* — are not expressible
deterministically today.

## The core decision

Ship the missing transforms as **pure ops**, per the evolution-doc precedent. No new node
kinds. One predicate mini-language across ops and (via a parser story) native `when`
conditions: the existing `Expr` formula engine, extracted into a public
`flux_lang::expr` module, extended with an `it` element variable and dotted field access.

**Why one mini-language:**
- Models emit JSON plans (L-20, decided by measurement) → a predicate is a **JSON string
  field**, the easiest thing to emit correctly. Structured `{field, op, value}` +
  `all_of`/`any_of` trees are ~4× the tokens and mangle worse under small models.
- `validate_expr_formula` already exists and gives actionable diagnostics; reusing it =
  static analyzer validation for `where` strings out of the box.
- One familiar surface. Not two mini-languages with different quoting and null rules.

**Rejected alternatives:**
- Structured JSON predicates (tokens + small-model failure).
- A jq-subset predicate engine (second mini-language, no existing validator).
- A generic `reduce{items, formula, init}` op (accumulator-variable protocol confuses
  small models, unanalyzable, violates *agent working language, not general-purpose
  language*). Every observed reduce need is covered by targeted aggregations below.

**Why not new node kinds:** each new node kind multiplies analyzer/runtime/render surface;
the catalog grew 7→43 already. Pure ops get native text for free (ordinary call syntax).
A node is only justified when the runtime must *understand* the semantics
([evolution.md §L-15 bar](flux-lang-evolution.md)) — none of these transforms qualify.

**Wire-format policy stays intact:** JSON is canonical for emission (L-20). The one text
story here (L-51) is authoring/reading ergonomics, not an emission-format change.

## Predicate representation (exact semantics)

Predicate-bearing ops take `where: "<expr formula>"`, evaluated per element with:

1. **`it`** bound to the element (via `ExprVal::from_json`). Nested field access:
   `it.author.username` — the tokenizer accepts `.` in identifier continuation position,
   resolution splits on `.`, descends objects jq-style.
2. **Missing/null → empty string `""`** (matches today's `ExprVal::from_json` null
   mapping): falsey, `as_num() = None`, `== ''` is true. Explicit null-fallback goes
   through the new `coalesce` op.
3. **Keep rule = `ExprVal::truthy`** (the runtime table: `null`/`false`/`0`/`""`/
   `"false"`/`"0"`/`[]`/`{}` falsey). Aligns with everywhere else in the language.
4. **Optional `vars` param** adds read-only named comparands (mirrors the `expr` node's
   `vars`): `filter({items: $issues, where: "it.milestone == m", vars: {m: $target}})`.
   Names must be identifiers; `it` is reserved.
5. **Bare-element predicates** work for scalar lists: `where: "it > 40000"`.
6. **Errors:** malformed formula / unknown function / undeclared variable fails the whole
   call with `validate_expr_formula`'s diagnostic (no partial output). Two special-cased
   hints for the small-model mis-emissions we will actually see: a formula starting with
   `.` gets *"element fields are `it.<field>`"*; one containing `$` gets *"symbols go in
   `vars`, elements are `it.<field>`"*.

Existing `by` params on `filter`/`sort`/`dedupe` are extended to accept dotted paths
(`by: "author.username"`) with the same missing→null rule — the cheap 80% case stays
formula-free.

## Op catalog

All ops: `effects: []`, `Risk::Low`, `Idempotent`, `deny_unknown_fields` schemars schemas
via `flux_spec::tool_input_schema`, JSON-string results — the existing `pure_spec` pattern
in `flux-tools/src/cognition.rs:56`. New module `flux-tools/src/transform.rs`, registered
into the force-on `cognition` group (`flux-tools/src/groups.rs:117`).

### Tier 1 — core transforms (story L-47)

| Op | Params | Semantics |
|---|---|---|
| `map` | `items`, one of `path` \| `expr`, `vars?` | Project each element: `path` plucks (dotted) field, missing→`null`; `expr` evaluates formula with `it` |
| `filter` (extend) | `items`, `where?`+`vars?` XOR `by?`+`equals?` | Existing kept; adds `where` formula mode; `by` accepts dotted paths |
| `flatten` | `items`, `depth?` (default 1, max 8) | Flatten nested arrays `depth` levels; non-array elements pass through |
| `skip` | `items`, `n` | Drop first n (mirror of `top`); replaces the bespoke `candidates_advance` pattern |
| `join` | `items`, `sep?` (default `"\n"`) | Stringify (strings as-is, others compact JSON) and join |
| `split` | `s`, `sep?` (default `"\n"`), `trim?` (default false) | Split string → JSON array; empty string → `[]` |

```flux
$names   = map({items: $issues, path: "author.username"})
$hot     = filter({items: $issues, where: "it.state == 'opened' && it.upvotes > 2"})
$all     = flatten({items: $pages})
$rest    = skip({items: $candidates, n: 1})
$report  = join({items: $lines, sep: "\n"})
$hosts   = split({s: $raw, sep: ","})
```

### Tier 2 — aggregations & boolean-emitters (story L-48)

| Op | Params | Semantics |
|---|---|---|
| `sum` | `items`, `path?` | Numeric sum (of `path` per element if given); non-numeric element → clear error |
| `count_by` | `items`, `path` | `[{key, count}]`, count desc, key asc tiebreak |
| `group_by` | `items`, `path` | `[{key, items}]`, first-seen key order (matches `dedupe`) |
| `any` | `items`, `where?`, `vars?` | `"true"`/`"false"` — some element satisfies; empty→`"false"` |
| `all` | `items`, `where?`, `vars?` | `"true"`/`"false"` — every satisfies; vacuously `"true"` on empty (doc loudly) |
| `has` | `items`, `value` | JSON-equality membership → `"true"`/`"false"` |

```flux
$by_sev = count_by({items: $findings, path: "severity"})
until all({items: $checks, where: "it.status == 'ok'"})
when has({items: $labels, value: "bug"})
```

`any`/`all`/`has` are the sanctioned replacement for bespoke boolean-emitters
(`candidates_empty` et al.) and compose with `analyze.rs:1004`'s call-condition rule
**today**, no parser work needed.

### Tier 3 — object & null kit (story L-49, no formula dependency)

| Op | Params | Semantics |
|---|---|---|
| `pick` | `items`, `keys` | Keep only `keys` (top-level). `items` may be one object **or** array of objects |
| `omit` | `items`, `keys` | Complement of `pick` |
| `merge_obj` | `objects` | Shallow merge array-of-objects, later keys win (distinct from list-concat `merge`) |
| `coalesce` | `values`, `default?` | First value that is not `null` and not `""`; else `default` (else `null`). 0/false are kept |
| `keys` / `values` | `item` | Object keys / values as arrays (deterministic order) |

```flux
$slim = pick({items: $issues, keys: ["iid", "title", "state"]})
$who  = coalesce({values: [$assignee, $author], default: "unassigned"})
```

### Tier 4 — regex (story L-50)

Rust `regex` crate: linear-time by construction, no backtracking → **no ReDoS**;
lookarounds/backrefs unsupported (the whitelist we want). Guardrails:
`RegexBuilder::size_limit(1 MiB)`, pattern ≤ 512 chars.

| Op | Params | Semantics |
|---|---|---|
| `regex_match` | `s`, `pattern` | `"true"`/`"false"` |
| `regex_extract` | `s`, `pattern`, `group?` (default 0), `all?` (default false) | First match (string or `null`); with `all: true`, an array of all matches |

```flux
$ver = regex_extract({s: $output, pattern: "v(\\d+\\.\\d+)", group: 1})
```

### Deferred (revisit on evidence)
Generic `reduce`; negative slicing (`skip`+`top` compose); `regex_replace` (expr `replace`
covers literal); `range` (counter loops exist); native `|>` pipe (P6/P8 precedent — pure
sugar over the @json-only `pipe` node; models emit JSON anyway).

## Expression-language extensions (story L-46)

Extract the engine — `Tok`, `tokenize_expr`, `eval_expr_value`, `expr_call_fn`,
`validate_expr_formula`, `ExprVal` (runtime.rs:3696-4200) — into a **public**
`flux_lang::expr` module (new `crates/flux-lang/src/expr.rs`). Runtime and analyzer
re-import it unchanged. `flux-tools` adds `flux-lang.workspace = true` (verified layer-legal:
`flux-lang` = L0, `flux-tools` = L2; `flux-lang` has no `flux-tools` dep; no cycle).

Extensions:

1. **Dotted variable access** — tokenizer admits `.` in ident continuation; resolution
   descends objects; missing/null → `Str("")`.
2. **`ExprVal::Obj(serde_json::Map)`** — objects stop stringifying (`from_json` currently
   degrades them to `Str`); truthy iff non-empty, `as_text` = compact JSON,
   `len(obj)` = key count. `List` already exists (L-35); it becomes producible/consumable.
3. **New builtins** (whitelist stays): `sum(xs)`, `any(xs)`, `all(xs)`, `has(xs, v)`,
   `join(xs, sep)`, `split(s, sep) -> List`, `first(xs)`, `last(xs)`, plus single-List
   overloads of `min(xs)` / `max(xs)`. `len` already counts lists (L-35).

Op-fn overlaps (`len`, `first`, `last`, `join`, `split`, `sum`, `any`, `all`, `has` exist
as both ops and expr builtins) are **deliberate and semantics-aligned**; conformance tests
in L-48 pin identical outputs. Docs present them as one vocabulary in two positions.

## Native-text ergonomics (story L-51)

Extend the parser so:

```flux
when $issue.state == 'opened' && $count > 3
$ok = $score >= 0.8
until len($queue) == 0
```

lower to `Expr` nodes with auto-built `vars`. Fallback rule: if the current
condition/bind-RHS parse leaves a comparison/boolean-operator tail, re-lex with the expr
tokenizer (extended to accept `$`-prefixed idents) and emit an `Expr`. No grammar
ambiguity: today any operator tail is an error path already. `format.rs` renders `Expr`
natively **only when the lowering is invertible** (vars map ≡ `{name: $name}` or dotted
read) — the same "sugar only when round-trips" rule already used for `$plan.kind`.

Analyzer nicety: op params with schema `"format": "flux-expr"` (the `where` / `expr`
params on the new ops) get literal-string validation via `validate_expr_formula` at
analysis time, so a malformed predicate surfaces before dispatch.

**Deferred:** native `|>` pipe. Pure authoring sugar over the @json-only `pipe` node;
adds infix machinery to a line-based statement grammar; P6/P8 precedent lets us add it
later on evidence.

## Story slicing

| Story | Scope | Depends on |
|---|---|---|
| [L-46](../stories/L-46-expr-engine-module-and-list-builtins.md) | Extract `flux_lang::expr`; dotted access; `Obj` variant; new list builtins | — |
| [L-47](../stories/L-47-core-transform-ops.md) | `map`, `filter.where`, `flatten`, `skip`, `join`, `split`; dotted `by` on filter/sort/dedupe | L-46 |
| [L-48](../stories/L-48-aggregation-predicate-ops.md) | `sum`, `count_by`, `group_by`, `any`, `all`, `has`; op↔expr conformance tests | L-46 |
| [L-49](../stories/L-49-object-null-kit-ops.md) | `pick`, `omit`, `merge_obj`, `coalesce`, `keys`, `values` | — (parallel) |
| [L-50](../stories/L-50-regex-ops.md) | `regex_match`, `regex_extract` with size limits | — (parallel) |
| [L-51](../stories/L-51-native-expr-conditions.md) | Native expr in condition + bind RHS; invertible format; `format=flux-expr` literal-string analyzer check | L-46 |
| [L-52](../stories/L-52-transforms-docs-examples-pass.md) | Docs pass; rewrite deterministic LLM-as-mapper patterns in examples; final regen sweep; closing gate | all above |

**Order:** L-46 first. L-49 + L-50 parallelizable from day 1. L-47 + L-48 after L-46
(parallel to each other). L-51 after L-46 (L-47/48 useful but not blocking). L-52 last —
public-docs pass at the end.

## Risks / open questions

1. **Truthiness divergence (pre-existing, becomes load-bearing).**
   `cognition::is_truthy` treats `"false"` / `"0"` strings as truthy; the runtime table
   does not. All *new* predicate paths use the runtime table. L-47 also aligns legacy
   `filter` truthy-mode to the runtime table with a CHANGELOG note — a `"false"` string
   field kept by `filter` today is almost certainly a user bug, and two truthiness tables
   in one op is a documentation trap.
2. **Namespace overlap by design.** Op ↔ expr-builtin pairs share names. Conformance
   tests pin identical semantics.
3. **`merge` vs `merge_obj`.** Do not overload `merge` (list-concat).
4. **Small-model mis-emissions.** `.state` (jq habit) and `$state` in where strings —
   error message special-cases both with rewrite hints; `format=flux-expr` analyzer check
   catches literal strings pre-execution.
5. **`map` vs `each` confusion.** Docs rule of thumb: `each` for effects/calls per item,
   `map` for pure projection.
6. **`all` vacuous truth on empty lists** in `until` guards can terminate loops early —
   documented on the op and in control-flow docs.
7. **flux-tools → flux-lang dependency.** Layer-legal (L2 → L0) but grows flux-tools'
   build. If build cost bites, extraction to a `flux-expr` crate is mechanical later.
