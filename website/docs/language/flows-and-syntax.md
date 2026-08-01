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
flow check-readme
  content = read("README.md")
  return content
```

```flux
# multi-flow file (a module)
flow fetch-and-grep
  hits = grep(glob: "*.rs", pattern: "TODO")
  return hits

flow summarize(text: String) -> String
  summary = task(role: "summarizer", task: "Summarize:\n{text}")
  return summary
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
when a
  when b
    bash("both true")
  else
    bash("a true, b false")
else
  bash("a false")
```

## Comments

Line comments start with `#` and run to end of line. There are no block comments. `#` has no
special meaning inside string literals.

```flux
x = read("a.txt")
```

## Symbols

All runtime values live in named symbols (lowercase, underscores allowed). A symbol is written bare:

```flux
flow read-notes
  result = read("some/file.txt")
  return result
```

The formatter emits the bare form for every name that can be spelled bare. Operator formulas are the
exception: their embedded expression grammar uses `$name` to distinguish symbol reads from its own
identifiers and function names.

Each binding creates a new immutable value. Binding the same symbol name later updates which value
that name resolves to; the earlier value remains in the session's versioned audit trail. Parameters
are declared without a sigil in the header and referenced the same way in the body.

For migration, the parser still accepts the older sigil and object-wrapped argument spellings. This
is the only compatibility-form example in the language guide; format the file to obtain canonical
source:

```text
# Compatibility spelling — accepted input, not canonical formatter output.
$result = read({path: "README.md"})
```

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
msg = "built {sha} in {elapsed}ms"
example = "use {{key: value}} syntax"
```

### Multi-line strings

A `"""` token opens a multi-line string. Everything up to the **next literal `"""`** is the value,
taken completely **verbatim**: no escape processing (`\n`, `\"` and `\\` are literal characters, not
escapes), no comment stripping (a `#` inside the block is content), and no indentation stripping
(the block's own indentation is part of the value — there is no dedent).

```flux
flow review-diff -> String
  diff = git_diff()
  prompt = """Analyse this diff and suggest improvements.
Focus on correctness, not style.

Diff:
{diff}"""
  notes = ai.reason(ask: prompt)
  return notes
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
hits = grep(glob: "*.rs", pattern: "TODO")
git_status()
```

A bare call (no bind) runs the operation for its side effects; the result is discarded from
the symbol table but still appears in the run trace.

### Named arguments

A multi-parameter operation takes **named arguments**, written brace-free in text. They lower to one
object node of parameter names in the AST:

```flux
flow named-arguments
  hits = grep(glob: "*.log", max_results: 50, pattern: "ERROR")
  page = read(limit: 100, offset: 200, path: "large.txt")
  src = read("README.md")
  return src
```

Where a name and the symbol holding its value are identical, write the name once — `grep(pattern,
glob)` is a **pun** for `grep(pattern: pattern, glob: glob)`. An operation with exactly one required
parameter accepts a bare value as sugar (`read("README.md")` above). Passing two or more bare
positional arguments is rejected by the analyzer: there is no positional convention.

## Binds

`name = <expr>` stores a result. The expression may be a call, a pure node, a value template,
another symbol, or a literal. An optional type annotation documents the expected type:

```flux
tests: TestResult = cargo_test(args: ["--workspace"])
ok = $score >= 0.8
scaled = $base * 1.2
```

Operator formulas in bind RHS positions lower to pure `expr` nodes. `$name` references become the
`expr.vars` map automatically, and dotted `$issue.state` reads object fields leniently inside the
formula. Annotations are preserved in the AST and used by analysis; they are optional everywhere.

One bind variant is spelled differently: prefixing a **call bind** with `memo` caches that operation
and canonical argument AST for the session, so the same call does not re-execute on later turns.
Changing the operation or arguments recomputes; non-call expressions use ordinary binds. `memo`
takes the same optional type annotation and `@effect(tag)` line as an ordinary bind:

```flux
memo schema = read("schema.sql")
```

See [Durability](./durability.md#memo--compute-once-per-session) for when the cache is invalidated.

## Native conditions

Condition positions accept the same native expression syntax:

```flux
when $count > 3
  return "enough"
repeat 10, until: all(items: checks, where: "it.status == 'ok'")
  poll()
```

For structured arrays, prefer pure `map`/`filter` projection over an `each` loop when no per-item IO
is needed. Keep `each` for work that dispatches calls per item, such as reading files or spawning
sub-agents.

### Effect annotations

An optional `@effect(tag)` line annotates the bind that follows it with an additional declared
semantic consequence. It feeds the analyzer's risk view but cannot replace or reduce the called
operation's host-declared effects and dispatch-time approval contract:

```flux
@effect(send_external)
report = generate_pdf(data)
@effect(delete)
gone = bash("rm -rf tmp/")
```

Valid tags: `pure`, `read`, `model`, `network`, `write_file`, `write_db`, `send_external`,
`delete`, `money`, `human_visible`. See
[Types & effects](./types-and-effects.md).

## return

```flux
return hits
return "done"
return null
```

`return` is an unconditional early exit from the **entire flow** — execution after it is
unreachable. To exit conditionally, put the `return` inside a branch:

```flux
when done
  return result
bash("continue working")
```

A `return` inside a `parallel` branch is rejected by the analyzer — bind a result in the
branch and return after the join instead.

## The `@json` escape

Every AST node kind has a native text spelling. The one-line `@json` escape — carrying a node's
compact JSON form — remains for the rare *shapes* the text grammar cannot express: a symbol name
that is not an identifier, a non-invertible `expr` formula, or a `jq` AST whose path string itself
uses brackets (native `items[0]` lowers to the canonical `.items.0` AST path) or whose input cannot
use field-access sugar:

```flux
@json { "kind": "bind", "name": "report.v2", "value": { "kind": "var", "name": "draft" } }
```

Treat `@json` as an escape hatch, not a preferred style — a plan written natively round-trips
through the formatter without it.

## Related docs

- [Control flow](./control-flow.md) — branching, iteration, routing, and fallback.
- [Pure data shaping](./pure-data.md) — transform values without dispatching IO.
- [Node reference](./node-reference.md) — JSON shapes for every node kind.
- [Editor setup](./editors.md) — highlighting, diagnostics, and completion while you write this syntax.
