---
title: Flows & syntax
description: The core Flux-Lang text syntax — file structure, flow headers, indentation, symbols, literals, interpolation, calls, binds, and return.
---

# Flows & syntax

This page is the text-syntax reference for `.flux` files: flow headers, indentation, symbols,
literals, calls, binds, returns, and the `@json` escape. The [node reference](./node-reference.md)
documents the same constructs in their JSON wire form.

## File structure

A `.flux` file is a sequence of one or more named flow definitions. The `flow` header is
**always required**, even for single-flow files — any valid snippet stays valid when pasted
into a larger file.

```flux
# single-flow file
flow check-readme
  $content = read("README.md")
  return $content
```

```flux
# multi-flow file (a module)
flow fetch-and-grep
  $hits = grep({pattern: "TODO", glob: "*.rs"})
  return $hits

flow summarize(text: String) -> String
  $summary = task({role: "summarizer", task: "Summarize:\n{text}"})
  return $summary
```

Each flow header starts at column 0. Blank lines and comments between flows are allowed. A file
with several flows is a **module**, which may also declare composite ops and program
declarations — see [Modules, composite ops & programs](./modules-and-programs.md).

## The flow header

```text
flow <name> [( <param>: <Type>, ... )] [-> <Type>]
```

- `<name>` — an identifier; `snake-case` or `snake_case` by convention.
- Parameters are a comma-separated list of `name: Type` — **no `$` prefix** in the header
  (they are declarations, not references).
- `-> Type` — an optional return type annotation.

```flux
flow check-ci
flow build-report(repo: String, branch: String) -> String
flow poll-until-done(url: String, timeout_ms: Number) -> Bool
```

A flow body may also open with a `goal "…"` line. It is accepted and ignored — a
human-readable statement of intent, not part of the plan.

## Indentation

Indentation is **2 spaces** per level. Tabs are rejected. There are no braces and no
semicolons; a block ends when the next non-blank line returns to the parent indentation level.

`else` sits at the same indent as its matching `when`:

```flux
when $a
  when $b
    bash("both true")
  else
    bash("a true, b false")   # else of the inner when
else
  bash("a false")             # else of the outer when
```

## Comments

Line comments start with `#` and run to end of line. There are no block comments. `#` has no
special meaning inside string literals.

```flux
# a full-line comment
$x = read("a.txt")   # an inline comment
```

## Symbols

All runtime values live in named symbols (lowercase, underscores allowed). A symbol is written bare:

```flux
flow read-notes
  result = read("some/file.txt")   # bind: result now holds the file contents
  return result                    # reference
```

The `$` sigil is an accepted **escape**, not a requirement. Write `$name` when the name collides with
a contextual keyword, or when you prefer the older spelling — most examples on this site still use it
and parse identically. The formatter emits the bare form for every name that can be spelled bare, so
that is the canonical surface.

Symbols are immutable once bound on a single execution path; rebinding stores a new value.
Parameters are declared without a sigil in the header and referenced the same way in the body.

## Literals

| Kind | Syntax | Example |
|---|---|---|
| String | double-quoted, or `"""…"""` to span lines | `"hello"` |
| Number | bare numeric | `42`, `3.14` |
| Bool | keyword | `true`, `false` |
| Null | keyword | `null` |
| Array | `[val, val, ...]` | `["a", "b", "c"]` |
| Object | `{key: val, ...}` | `{adapter: "local", trials: 3}` |

A `"…"` string is single-line — embed newlines with `\n` escapes, or use the
[`"""…"""` form](#multi-line-strings) below, which is the better choice for anything long. Object
and array literals are valid inside call arguments. Layout **inside** a delimiter — an argument list,
object, or array — is whitespace, so those may span lines and may carry a trailing comma; it is the
statement grammar *outside* delimiters that is line-based. Inside a call argument list, `{` always
starts an object, never a block; blocks are only introduced by flow-control keywords on their own
line.

An object or array whose leaves include symbols or expressions is a **value template** rather
than a literal — see [Pure data](./pure-data.md).

### String interpolation

Any string literal may embed `{symbol}` placeholders, substituted from bound symbols at
evaluation time. Unbound names are left verbatim. Double the braces to emit a literal brace:

```flux
$msg     = "built {sha} in {elapsed}ms"
$example = "use {{key: value}} syntax"   # outputs: use {key: value} syntax
```

### Multi-line strings

A `"""` token opens a multi-line string. Everything up to the **next literal `"""`** is the value,
taken completely **verbatim**: no escape processing (`\n`, `\"` and `\\` are literal characters, not
escapes), no comment stripping (a `#` inside the block is content), and no indentation stripping
(the block's own indentation is part of the value — there is no dedent).

```flux
flow review-diff -> String
  $diff = git_diff()

  $prompt = """Analyse this diff and suggest improvements.
Focus on correctness, not style.

Diff:
{diff}"""

  $notes = ai.reason({ask: $prompt})
  return $notes
```

Its terminator is found by scanning for the next `"""` rather than by tracking indentation, so a
`"""` block spans physical lines without any enclosing delimiter — unlike an argument list or object,
which spans lines only because it is already inside one. It works in **every** position a `"…"` string
does: a bind value, a call argument,
a `lit` nested in an object or array, a value-template leaf, and the natively spelled string fields
(`fmt`'s template, `assert`'s message, `ctx`'s purpose, `route`'s case label).

`{symbol}` interpolation applies exactly as it does to a normal string — interpolation is a property
of the value, not of the spelling used to write it.

Prefer this form for anything long: prompts, embedded JSON, diffs, file contents. Removing escaping
as a failure mode is the point of the feature.

:::note Three things a `"""` block cannot contain
Because the terminator is "the next literal `\"\"\"`", the content cannot itself contain `"""`, and
cannot **end** with a `"` (that quote would merge with the closing delimiter into an ambiguous run).
It also cannot contain a carriage return: `\r\n` is normalized to `\n` inside triple-quoted content,
so a `\r` would be silently lost on the way back through the parser. All three are rare in real
payloads. The formatter detects them and falls back to the escaped single-line spelling
automatically, so round-tripping is never unsafe — a small set of inputs simply don't get the nicer
spelling.
:::

## Calls

Both call forms are equivalent — inline `op(args)` and the statement form `do <op> <args>`:

```flux
$hits = grep({pattern: "TODO", glob: "*.rs"})
do git_status
```

A bare call (no bind) runs the operation for its side effects; the result is discarded from
the symbol table but still appears in the run trace.

### Named arguments

A multi-parameter operation takes **named arguments** — one object of parameter names, written either
brace-free or braced. Both spellings lower to the same single object argument; the brace-free one is
what the formatter emits, so it is canonical:

```flux
flow named-arguments
  hits = grep(pattern: "ERROR", glob: "*.log", max_results: 50)
  page = read({path: "large.txt", limit: 100, offset: 200})
  src = read("README.md")
  return src
```

Where a name and the symbol holding its value are identical, write the name once — `grep(pattern,
glob)` is a **pun** for `grep(pattern: pattern, glob: glob)`. An operation with exactly one required
parameter accepts a bare value as sugar (`read("README.md")` above). Passing two or more bare
positional arguments is rejected by the analyzer: there is no positional convention.

## Binds

`$name = <expr>` stores a result. The expression may be a call, a pure node, a value template,
another symbol, or a literal. An optional type annotation documents the expected type:

```flux
$tests: TestResult = cargo_test({args: ["--workspace"]})
$ok = $score >= 0.8
$scaled = $base * 1.2
```

Operator formulas in bind RHS positions lower to pure `expr` nodes. `$name` references become the
`expr.vars` map automatically, and dotted `$issue.state` reads object fields leniently inside the
formula. Annotations are preserved in the AST and used by analysis; they are optional everywhere.

One bind variant is spelled differently: prefixing a bind with `memo` caches its value for the whole
session, so the op does not re-execute on later turns. It takes the same optional type annotation
and `@effect(tag)` line as an ordinary bind:

```flux
memo $schema = read("schema.sql")
```

See [Durability](./durability.md#memo--compute-once-per-session) for when the cache is invalidated.

## Native Conditions

Condition positions accept the same native expression syntax:

```flux
when $count > 3
  return "enough"

repeat 10
  until all({items: $checks, where: "it.status == 'ok'"})
  do poll
```

For structured arrays, prefer pure `map`/`filter` projection over an `each` loop when no per-item IO
is needed. Keep `each` for work that dispatches calls per item, such as reading files or spawning
sub-agents.

### Effect annotations

An optional `@effect(tag)` line annotates the bind that follows it with a declared semantic
effect, which drives risk scoring and approval:

```flux
@effect(send_external)
$report = generate_pdf($data)

@effect(delete)
$gone = bash("rm -rf tmp/")
```

Valid tags: `pure`, `read`, `model`, `network`, `write_file`, `write_db`, `send_external`,
`delete`, `money`, `human_visible`. See
[Types & effects](./types-and-effects.md).

## return

```flux
return $hits      # end the flow with a value
return "done"     # literal return value
return            # return null
```

`return` is an unconditional early exit from the **entire flow** — execution after it is
unreachable. To exit conditionally, put the `return` inside a branch:

```flux
when $done
  return $result
# reached only when $done was falsey
bash("continue working")
```

A `return` inside a `parallel` branch is rejected by the analyzer — bind a result in the
branch and return after the join instead.

## The `@json` escape

Every AST node kind has a native text spelling. The one-line `@json` escape — carrying a node's
compact JSON form — remains for the rare *shapes* the text grammar cannot express: a symbol name
that is not an identifier, a non-invertible `expr` formula, or a `jq` with a bracket path or
non-symbol input:

```flux
@json {"kind": "bind", "name": "report.v2", "value": {"kind": "var", "name": "draft"}}
```

Treat `@json` as an escape hatch, not a preferred style — a plan written natively round-trips
through the formatter without it.

## Related docs

- [Control flow](./control-flow.md) — branching, iteration, routing, and fallback.
- [Pure data shaping](./pure-data.md) — transform values without dispatching IO.
- [Node reference](./node-reference.md) — JSON shapes for every node kind.
- [Editor setup](./editors.md) — highlighting, diagnostics, and completion while you write this syntax.
