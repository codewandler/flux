# Flux-Lang — Language Design

This document is the authoritative specification for the Flux-Lang text syntax: the
human-writable, editor-friendly form of a flow. It covers grammar, every node kind,
the relationship to the JSON wire format, and the toolchain.

---

## Implementation status (read first)

The text syntax **is built**: `flux_lang::parse::parse(&str) -> Result<DraftAst>` (one flow),
`flux_lang::parse::parse_program(&str) -> Result<Module>` (a whole `.flux` **module**: multiple
`flow`s plus `agent`/`channel`/`datasource`/`trigger`/`journey`/`op` declarations), and
`flux_lang::format::format(&DraftAst) -> String`, with `parse(format(ast)) == ast` — native
spellings for **every node kind**, with a single-line `@json <compact-json>` escape remaining only
for shapes the grammar cannot express (non-identifier names (L-18), non-invertible `expr`
formulas, bracket-path `jq`, all-literal `obj`/`list` templates); property-tested
(`tests/roundtrip_property.rs`).

The lossless rowan CST is the **sole accepting parser**. `parse` and `parse_program` run the tolerant
lexer/CST once, then strictly refuse any recovered diagnostic or `ERROR` token before lowering the
validated tree to `DraftAst`/`Module`. Editor tooling uses that same tolerant tree while a document
is incomplete, preserving comments and exact ranges; there is no second source parser behind the
strict APIs.

Body sections below marked **aspirational** describe *target* syntax the parser does **not**
accept today; everything unmarked is implemented.

- **Native text** (markers `=` bind · `do <op> <args>` effectful call · `+=` `ctx_append`): `bind`,
  `call` (bare `do …` or inline `op(…)`), `var` (`$x`), `lit` (JSON), `return`, `when`/`else`, `unless`,
  `each`, `repeat`, `seq`, the context-pack nodes **`ctx`** / **`ctx_append`**, `@effect(tag)` bind
  annotations, and (added P6) the Tier-1 control-flow blocks **`match`**, **`route`**, **`fallback`**,
  **`loop`**, **`timeout`**, **`budget`**, the capability scope **`with_tools`**,
  the inline **`fmt("…")`** node, and **`$var.path`** field-access sugar (lowers to a `jq` node); and
  (added P8) the value-template constructors **`obj`** (`{ k: expr }`) / **`list`** (`[ expr ]`) plus the
  **`assert`**, **`retry`**, and **`parallel`** blocks. Native operator formulas are accepted in bind
  RHS and condition positions (`when $count > 3`, `$ok = $score >= 0.8`) and lower to `expr` nodes.
  Flow header carries optional
  `name`/`params`/`returns`; a leading `goal "…"` line is accepted and ignored (not part of the
  AST/round-trip). The actual P6/P8 spellings are documented in
  [§ Native control-flow forms (P6)](#native-control-flow-forms-p6) below. The CST/LSP pass
  (L-60..L-63, 2026-07) closed the remaining coverage gap with native spellings for **`memo`**,
  **`once`**, **`checkpoint`**, **`await`**, **`confirm`**, **`throttle`**, **`debounce`**,
  **`verify`**, **`peek`**, **`parse(…)`**, **`try`/`catch`**, **`race`**, **`scope`/`finally`**,
  **`saga`** (`step`/`undo`), **`pipe`**, and **`thing`** — every node kind now has a native form,
  each documented in its section below.
- **`@json` escape** (pathological shapes only): a bind/memo whose symbol name is not an
  identifier, a non-invertible `expr` formula (e.g. one using the expr function library), any `jq`
  whose input is not a plain `$var` or whose path uses an array index (`.items[0]`), and all-literal
  `obj`/`list` templates — those round-trip through `@json`. Beware: writing `expr(…)`, `peek(…)`,
  or `jq(…)` call-style in text parses as an ordinary **op call** named `expr`/`peek`/`jq`, *not*
  the pure node — only `fmt(…)` and `parse(…)` are special-cased; the native `jq` spelling is the
  `$var.path` sugar and the native `peek` spelling is the keyword form `peek $x`.
- **Multi-line strings** (L-39, implemented): a `"""…"""` block — content taken **verbatim**, no
  escaping, no dedent — usable anywhere a string literal is valid (bind values, call args, `lit`
  values at any nesting depth inside an object/array, value-template leaves, and the natively
  spelled `fmt`/`assert`-message/`ctx`-purpose/`route`-case-label strings). `format` emits it
  automatically for any string containing a newline; see [§ Multi-line strings](#multi-line-strings)
  below for the full grammar.
- **Aspirational** (described below as the *target* language, **not** yet parsed): comma-form named
  arguments in call argument lists (`grep("ERROR", glob: "*.log")` — the shipped form is a single
  object argument, see [§ Named arguments](#named-arguments)); comma-kwarg flow-control headers
  (`retry 3, backoff: exponential`); multi-line *literals* inside call arguments other than a
  `"""…"""` string (e.g. a multi-line `{…}` object — the parser is otherwise strictly line-based);
  `@kind(…)` thing references (the implemented spelling is `thing <kind> <selector> "…"`);
  file-scope `type`/union declarations; and the `block`/`watch` spellings (the implemented keywords
  are `seq` and `loop`). The AST type is **`DraftAst`** (this doc historically said `FlowAst`,
  which does not exist).

---

## Motivation and scope

Flux-Lang exists at two levels:

- **Programmatic / storage format** — JSON (`DraftAst` via serde). Used by SDK/runtime layers,
  authored-flow persistence, replay, and host-derived execution records. It is not model output.
- **Text format** — `.flux` files. The human-writable, version-controllable surface.
  This document specifies the text format.

The two formats are semantically identical: every `.flux` file compiles to exactly the
same `DraftAst` that the JSON wire format expresses. The text format adds nothing that
the JSON format cannot represent; it only makes flows readable and writable by humans.

The `render.rs` terminal display (box-drawing tree) is a third, separate thing: it is
read-only output for inspection, not a format you write.

---

## File structure

A `.flux` file is a sequence of one or more named flow definitions (plus, at the module level,
optional `permissions`/`agent`/`channel`/`datasource`/`trigger`/`journey`/`op` declarations). The `flow` header
is **always required** — even for single-flow files. This keeps the format unambiguous
for parsers and formatters, and means any `.flux` snippet is valid in a multi-flow file
without modification. `parse` reads a single flow; `parse_program` reads a whole module
(a file with several `flow`s is a module).

```flux
# single-flow file
flow check-readme
  $content = read("README.md")
  return $content
```

```flux
# multi-flow file (a module — parsed by parse_program)
flow fetch-and-grep
  $content = read("README.md")
  $hits    = grep({pattern: "TODO", glob: "*.rs"})
  return $hits

flow summarise(text: String) -> String
  $summary = task({role: "summariser", task: "Summarise:\n{text}"})
  return $summary
```

Each flow starts with a `flow` header at column 0; blank lines and comments between flows are
allowed (blank lines are not required separators).

### Module declarations

Module declarations (`agent`/`channel`/`datasource`/`trigger`/`journey`) start at column 0 with the
keyword and a single-identifier name. The singleton top-level `permissions` declaration takes no
name. Except for `journey` (whose body is an `agent` attribute plus an inline `flow` block), a
declaration body is a **flat list of `key value` attribute lines**, all at one indentation level — no
nested blocks. Keys the decl kind knows become typed fields (`permissions`: `allow`/`deny`; `agent`:
`model`/`tools`/`datasources`/`description` plus `allow`/`deny`; `channel`/`datasource`: `kind`,
defaulting to the decl name, plus `datasource`'s `path`; `trigger` accepts *only*
`on`/`run`/`agent`); every other agent/channel/datasource key is collected into the decl's `settings`
object.

Capability lists contain exact operation names. A missing `allow` is represented as `None` and
inherits the parent/default set; an explicit `allow []` is represented as `Some([])` and admits no
effectful operations at that layer. Dotted operation names must be quoted:

```flux
permissions
  allow [search, "ai.reason", send]
  deny [write, bash]

agent guide
  tools [search]
  allow [search, "ai.reason", send]
```

A setting value is one of: a quoted string, a number, `true`/`false`/`null`, a bare identifier
(kept as a string), a `[a, b]` list, a `{ k: v }` record (lists and records nest), or a **secret
reference**:

```flux
channel slack
  kind slack
  bot_token secret "SLACK_BOT_TOKEN"
  mode socket
```

`secret "ENV_NAME"` compiles to the marker object `{"$secret": "ENV_NAME"}` in the decl's settings
— the name of the environment variable the host resolves at load time. Plaintext secrets are never
written inline in `.flux` text.

### Composite op declarations

A module may also declare reusable custom ops with `op`. A composite op has typed params, optional
metadata, and an ordinary Flux-Lang body. It is callable like any other op from flows in the same
module, but its inner calls still run through the normal safety envelope.

```flux
op repo-health(path: String, prior: Ctx) -> Health
  description "Check git state and summarize failures"
  risk "medium"
  idempotency "idempotent"
  effects [read, process, local_system]
  expose true

  $status = git_status()
  $tests = cargo_test({args: ["--workspace"]})
  ctx $pack
    purpose "repo-health"
    budget 8000
    include $prior, $status, $tests
  return {status: $status, tests: $tests}
```

The supported metadata keys are `description`, `risk`, `idempotency`, `effects`, `limits`, `expose`,
and `view`. `await` is rejected inside composite ops in v1, and direct or indirect recursion is invalid.

### Flow header

```
flow <name> [( <param>, ... )] [-> <type>]
```

- `<name>` — identifier, `snake-case` or `snake_case` by convention
- `(<param>, ...)` — optional comma-separated parameter list; each param is
  `name: Type` (no `$` prefix on parameters — they are declarations, not references)
- `-> <type>` — optional return type annotation

Examples:

```flux
flow check-ci
flow build-report(repo: String, branch: String) -> String
flow poll-until-done(url: String, timeout_ms: Number) -> Bool
```

---

## Indentation

Indentation is **2 spaces** per level by convention. The formatter always writes 2 spaces; the
parser requires consistent indentation within each block (every statement of a block at the same
column). Tabs are rejected. There are no braces and no semicolons.

A block ends when the next non-blank line returns to the parent indentation level.
This is the only block-termination rule — there are no `end` keywords.

### else / catch at same indent as their opener

`else` is always at the same indentation level as its matching `when`:

```flux
when $ok
  bash("echo yes")
else
  bash("echo no")
```

Nested `when`: the `else` belongs to whichever `when` is at the same indent level:

```flux
when $a
  when $b
    bash("both true")
  else
    bash("a true, b false")   # else of inner when
else
  bash("a false")             # else of outer when
```

The same rule applies to `catch` relative to its `try`, and to `finally` relative to its `scope`:
the arm keyword sits at the same indentation level as its opener.

---

## Symbols

All runtime values live in named symbols. A symbol reference is written `$name`
(lowercase, underscores allowed). The `$` sigil is mandatory on every symbol
*reference* in the body — it is the unambiguous signal that something is a runtime
value, not a keyword or op name.

```flux
$result = read("some/file.txt")   # bind: $result now holds the file contents
return $result                    # reference: pass $result to return
```

Symbols are immutable once bound within a single execution path. Rebinding the same
name in a different branch is allowed (the branches are independent paths).

**Parameters** are declared without `$` in the flow header (`name: Type`) but
referenced with `$` in the body (`$name`). This mirrors every mainstream language:
you declare `fn f(x: i32)` but write `x + 1` in the body — Flux-Lang uses `$x` in the
body to keep symbols visually distinct from keywords.

---

## Comments

Line comments start with `#` and run to end of line. Block comments are not supported.

```flux
# this is a comment
$x = read("a.txt")   # inline comment
```

`#` has no special meaning inside double-quoted string literals.

---

## Literals

| Kind | Syntax | Example |
|---|---|---|
| String | double-quoted | `"hello"` |
| Multi-line string | triple-quoted, verbatim | `"""` + real newlines, no escaping + `"""` — see [§ Multi-line strings](#multi-line-strings) |
| Number | bare numeric | `42`, `3.14` |
| Bool | bare keyword | `true`, `false` |
| Null | bare keyword | `null` |
| Array | `[val, val, ...]` | `["a", "b", "c"]` |
| Object | `{key: val, ...}` | `{adapter: "local", trials: 3}` |

Object literals in expression position (e.g. as a call argument) use `{key: val}`
syntax. Inside a call argument list, `{` always starts an object literal, never a
block. Blocks are only introduced by flow-control keywords on their own line.

### String interpolation

Any string literal may embed `{symbol}` placeholders. The runtime substitutes the
symbol's current value at execution time:

```flux
$msg    = "built {sha} in {elapsed}ms"
$prompt = "Summarise this:\n{content}"
```

To emit a literal brace in output, double it: `{{` produces `{`, `}}` produces `}`:

```flux
$example = "use {{key: value}} syntax"   # outputs: use {key: value} syntax
```

The JSON wire format uses the same `{sym}` / `{{` / `}}` convention inside string
values — there is no difference between the text and wire formats for interpolation.

### Multi-line strings

A `"""` token opens a multi-line string literal (L-39). Everything between the opening `"""` and
the **next literal `"""`** is the string's content, taken completely **verbatim**: no escape
processing (`\n`, `\"`, `\\` are literal characters, not escapes), no comment stripping (a `#`
inside the block is content, not a comment start), and no indentation stripping (the block's own
indentation, if any, is part of the value — there is no dedent). This is deliberately the simplest
possible rule: it removes escaping as a failure mode entirely, which is the point (see the story's
"why now" — a fine-tuned planner's dominant failure was breaking multi-KB JSON-string payloads with
literal newlines).

```flux
$prompt = """Analyse this diff and suggest improvements.
Focus on correctness, not style.

Diff:
{diff}"""
```

The block may span any number of physical lines and works in **every** position a `"…"` string can:
a bind value, a call argument, a `lit` value nested inside an object/array, a value-template leaf,
or any of the natively-spelled string fields (`fmt("…")`'s template, `assert`'s message, `ctx`'s
purpose, `route`'s case label). `{symbol}` interpolation applies exactly as it does to a normal
string — interpolation is a property of the *value*, not the spelling used to write it.

**Grammar note (why delimiter-based, not indentation-based):** the block's end is found by scanning
forward for the next `"""`, not by dedent/column tracking — this is a lexer-level rule (see
`flux_lang::parse`'s `preprocess`), independent of the line-based statement grammar everywhere else,
so a `"""` block is the one construct allowed to span multiple physical lines.

**Known limitation (accepted, documented):** because the terminator is "the next literal `\"\"\"`",
content cannot itself contain `"""`, and cannot **end** in a `"` character (that final quote would
merge with the closing delimiter into an ambiguous run of 4+ quotes). Both are vanishingly rare in
real payloads (source code, diffs, prose); `format` detects them and falls back to the standard
escaped single-line spelling automatically, so `parse(&format(&ast)) == ast` always holds — there is
no case where round-tripping is unsafe, only a small set of inputs that don't get the nicer
spelling.

`format_compact` (the display-only, non-round-tripping preview variant) never emits the multi-line
spelling — it always uses the escaped single-line form, so a compact plan preview stays visually one
line per statement.

### Inline object literals

Object (and array) literals are valid inside call arguments — **on one line**. The parser is
strictly line-based: a statement is a single line, so a multi-line literal inside a call is a parse
error. (This is about the object/array *shape* — one field per line, closing brace on its own line.
A `"""…"""` string **value** nested inside a one-line object literal is fine and may itself contain
real newlines; see [§ Multi-line strings](#multi-line-strings) — that block is the one exception to
"statement = one physical line", handled at the lexer level, not the object/array grammar.)

```flux
$result = eval_run({adapter: "terminal-bench", tasks: ["chess-best-move"], trials: 1, agent_timeout: 180})
```

*(Aspirational)* the multi-line spelling — contents indented 2 spaces deeper, closing `)` on its
own line — is **not implemented**:

```flux
# ASPIRATIONAL — does not parse today
$result = eval_run({
  adapter: "terminal-bench",
  trials:  1
})
```

---

## Calls and binds

### Bare call (result discarded)

```flux
git_stage(["."])
git_commit("chore: bump version")
```

### Bind (result stored)

```flux
$hits    = grep({pattern: "TODO", glob: "*.rs"})
$content = read("README.md")
```

### Named arguments

Named arguments are passed as **a single object argument** whose keys name the op's parameters
(story L-09). This is the one convention for multi-parameter calls; a sole-required-param op
accepts a bare value as sugar.

```flux
$hits = grep({pattern: "ERROR", glob: "*.log", max_results: 50})
$page = read({path: "large.txt", limit: 100, offset: 200})
$src  = read("README.md")            # sole-required-param sugar
```

Two or more bare positional arguments is the deprecated positional form — the analyzer rejects it.

*(Aspirational — not implemented)* a comma-separated `key: value` form appended after positional
args (`grep("ERROR", glob: "*.log")`) does **not** parse; nor do comma-kwarg flow-control headers
(`retry 3, backoff: exponential` / `race timeout: 5000`). The implemented flow-control headers use
**space-keyword** tokens in fixed order (see [§ Native control-flow forms
(P6)](#native-control-flow-forms-p6)):

```flux
retry 3 backoff exponential delay 500 -> $out
loop for 10000 every 1000
```

### Memo (cross-turn cache)

A `memo` node binds once per session: on subsequent turns the cached value is reused without
re-executing the op. The spelling is `memo` + an ordinary bind — including the optional type
annotation and a preceding `@effect(tag)` line:

```flux
memo $schema = read("schema.sql")

@effect(read)
memo $survey: String = read("big.log")
```

---

## Effect annotations

An optional `@effect(name)` annotation precedes the **bind** it annotates (a bare call cannot
carry one — the parser rejects `@effect` on anything but a bind):

```flux
@effect(send_external)
$report = generate_pdf($data)

@effect(delete)
$gone = bash("rm -rf tmp/")
```

Valid effects: `pure`, `read`, `model`, `network`, `write_file`, `write_db`,
`send_external`, `delete`, `money`, `human_visible`. (A legacy `calendar` tag still parses but is
deprecated — declare `send_external` or `write_db` instead.)

The `@` prefix is unambiguous: it introduces an annotation or a thing-reference and
is never used as an operator.

---

## Control flow

### when / else

```flux
when $ok
  bash("echo yes")
else
  bash("echo no")
```

The condition is any expression — a symbol, a call, or a bool literal. Calling an op
directly as the condition (without binding first) is valid:

```flux
when fetch_status($url)
  bash("echo up")
```

The `else` branch is optional.

### unless

Sugar for `when !cond`. Use for guard clauses:

```flux
unless $already_built
  bash("cargo build")
```

`unless` does not support an `else` branch. Use `when` if you need one.

### repeat

Counter-driven bounded loop. The count is required.

```flux
repeat 5
  bash("poll.sh")
```

With an early-exit condition. `until` is written on its own line as the **first**
statement of the body; it is a stop-when-true guard evaluated **after** each iteration:

```flux
repeat 10
  until $done
  $done = bash("poll.sh")
```

### each

List-driven loop. Prefer over `repeat` when iterating a known list.

```flux
each $f in $files
  $text = read($f)
```

Collecting results — the result of each iteration is the value of the last expression
in the body. If the source list is empty, `$collect` is bound to `[]`:

```flux
each $f in $files -> $results
  read($f)
```

Flattened collect — each iteration yields a list; they are concatenated:

```flux
each $dir in $dirs -> flat $all_files
  glob({pattern: "*.rs", path: $dir})
```

### watch *(aspirational spelling — the implemented keyword is `loop`)*

Time-bounded iteration is implemented as the **`loop`** block (see [§ loop / timeout /
budget](#loop--timeout--budget)); the `watch` spelling and its comma-kwarg header
(`watch for: 10000, every: 1000`) never landed and do **not** parse. The real spelling:

```flux
loop for 30000 every 2000 -> $last
  until $done
  $done = bash("health-check.sh")
```

`until` is the optional first body line (a stop-when-true guard evaluated after each iteration);
`-> $name` optionally captures the last iteration's result.

---

## Native control-flow forms (P6)

These are the **as-implemented** text spellings of the Tier-1 control-flow nodes (added P6). They use
positional headers and the same 2-space block rule as `when`/`each`; `default`/`case`/`branch` arms sit one
indent under the header, their bodies one indent further.

### match

Multi-way exhaustive branch on a **bound** value (a `$var` or literal — to branch on an op result or a
field, bind it first: `$k = $request.kind`). Each `case <value>` runs when `subject == value` (JSON equality);
an optional `default` runs when none match.

```flux
$kind = $request.kind
match $kind
  case "chat"
    $answer = $request.text
  case "error"
    $answer = $request.text
  default
    $answer = handle_request($request)
```

### route

Like `match`, but the subject is a `selector` op (typically model-backed) and the arms are string
**labels** (`case "<label>"`). The model picks *which* declared branch runs, never *what*.

```flux
route classify($utterance)
  case "bug"
    do file_bug $utterance
  case "feature"
    do file_feature $utterance
  default
    do triage $utterance
```

### fallback

Ordered "first branch that succeeds wins". Each `branch` is tried in turn; `-> $bind` names the winning
result. (Branches have no header — they are bare `branch` + body.)

```flux
fallback -> $value
  branch
    $value = read("cache.json")
  branch
    $value = fetch($url)
```

### loop / timeout / budget

`loop` is a time-bounded loop (`for`/`every` in ms; optional `until` as the first body line, like
`repeat`); `timeout` bounds its body by wall-clock ms; `budget` caps the number of dispatches. All three
take an optional `-> $bind`.

```flux
loop for 30000 every 2000 -> $last
  until $done
  $done = bash("health-check.sh")

timeout 5000 -> $out
  $out = bash("slow.sh")

budget 10 -> $used
  do retryable_step
```

> The implemented loop keyword is **`loop`** (the aspirational `watch` spelling above is superseded).

## Native value templates and blocks (P8)

**Value templates** — a record or list whose leaves may be variables/expressions. `{ … }` / `[ … ]`
is a template (`obj`/`list`) when it is **not** valid JSON (an unquoted key or a `$var`/expr leaf);
pure JSON stays a `lit`. Each value is an expression (`$var`, `$v.path`, `op(…)`, `fmt(…)`, nested
templates), so a record assembles from computed symbols:

```flux
$r = { ok: true, n: $count, intent: $extract.intent, items: [$a, $b] }
return { status: "done", refs: $refs }
```

Keys are barewords when identifier-safe, else JSON-quoted (`{ "a-b": $x }`). An all-literal or empty
template has no native form and round-trips via `@json`.

**`assert <cond> [, "<message>"]`** — a one-line guard (the first top-level `,` begins the message):

```flux
assert $hits, "grep returned no results"
assert ok($a, $b)
assert $score >= 0.8, "score too low"
```

**`retry <max> [backoff <ident>] [delay <ms>] [-> $bind]`** + body — space-keyword tokens in fixed
order (`backoff` is `none`/`linear`/`exponential`):

```flux
retry 3 backoff exponential delay 500 -> $out
  do flaky_step
```

> Note: the `retry 3, backoff: exponential` comma form shown under [§ Named arguments](#named-arguments)
> is aspirational; the implemented spelling is the space-keyword form above.

**`parallel`** + indented `branch $name` arms — each branch runs concurrently and binds its result to
`$name` (no `default` arm):

```flux
parallel
  branch $readme
    $readme = read("README.md")
  branch $todos
    $todos = grep("TODO")
```

---

## Sequencing and piping

### seq

A sequential block that optionally binds its final result. The implemented keyword is **`seq`**
(the proposed `block` rename never landed and does not parse):

```flux
seq -> $result
  bash("echo one")
  $two = bash("echo two")
```

A `seq` with no result binding is valid:

```flux
seq
  git_stage(["."])
  git_commit("chore: update")
```

### pipe

Each step's output is passed as the first argument of the next step. The final
step's output is the pipe's result. The header is `pipe [-> $bind]`, followed by one indented
call per line:

```flux
pipe -> $hits
  read("log.txt")
  grep("ERROR")
```

A `pipe` with a single step is valid (equivalent to a bare call). A native `|>` operator remains
deferred.

---

## Concurrency

### parallel

Run independent branches concurrently. Each branch is introduced by a `branch $name`
arm (mirroring `fallback`'s `branch` arms), with the branch body indented one level further.

The **result of a branch** is the value of the last expression evaluated in its body.
After the `parallel` block, each branch name is a bound symbol.

```flux
parallel
  branch $readme
    $readme = read("README.md")
  branch $todos
    $todos = grep("TODO")
```

After this block, `$readme` holds the file contents and `$todos` holds the grep hits.

A symbol bound *inside* a branch body (other than its implicit result) is not
visible outside that branch. A `parallel` with one branch is valid (degenerates to a
sequential bind); a `parallel` with zero branches round-trips as an empty block.

### race

Run branches concurrently; the first branch to complete **successfully** wins. The deadline is
required and positional; the header is `race <timeout_ms> [-> $bind]`, followed by the same
`branch $name` arms as `parallel`:

```flux
race 5000 -> $result
  branch $fast
    bash("fast-path.sh")
  branch $slow
    bash("slow-path.sh")
```

If every branch fails, the node errors with a joined branch error (distinct from a timeout); if
the deadline expires first, it errors with a timeout. Losing branches' dispatched steps stay
counted and traced.

---

## Error handling

### try / catch

`try` + an indented body, then an optional `catch [$err]` arm (at the same indent as the `try`)
+ an indented handler:

```flux
try
  bash("might-fail.sh")
catch $err
  bash("echo fallback: {err}")
```

- `catch $err` binds the error message string to the named symbol; a bare `catch` runs the
  handler without binding
- The `catch` arm is optional; a `try` with no handler suppresses errors silently
- If the handler also errors, that error propagates

### retry

Retry the body on failure up to `max` times. The header uses **space-keyword** tokens in fixed
order (the comma-kwarg form `retry 3, backoff: exponential, delay: 500` is aspirational and does
not parse):

```flux
retry 3 backoff exponential delay 500 -> $out
  bash("flaky.sh")
```

- `max` (positional, required) — maximum attempts including the first
- `backoff none | linear | exponential` — default `none`
- `delay <ms>` — base delay in milliseconds; when omitted the runtime defaults to `500`
- `-> $name` — binds the last expression of the body on success
- Fatal errors (policy denial, unknown op) are never retried
- A denied `confirm` inside a `retry` body is **not** retried

Do not also bind the result inside the body — the header binding captures it:

```flux
# correct: header binding
retry 3 -> $out
  bash("flaky.sh")

# correct: side-effects only, no binding needed
retry 3
  bash("flaky.sh")
```

---

## Human-in-the-loop

### confirm

Explicit approval gate. The `--yes` flag and the TUI modal satisfy it automatically. The header
is `confirm "<message>" [risk <level>]` + an optional indented body (the comma-kwarg spelling
`confirm "…", risk: high` does not parse):

```flux
confirm "Delete all temp files?" risk high
  bash("rm -rf tmp/")

confirm "Proceed?"
```

- `message` (required)
- `risk`: `low | medium | high | critical` — default `medium` (omitted from the header)
- Body runs only on approval; denial causes the node to error
- A `confirm` with **no body** is valid — a pure gate with no conditional action

---

## Rate limiting and debouncing

Both headers use space-keyword tokens (the comma-kwarg forms shown in earlier drafts do not
parse). See [`reference.md`](reference.md) for full semantics.

### throttle

At most `max` **op dispatches** inside the body per sliding `window_ms`; the bucket is tracked in
the session store, atomically, keyed by the required name. The header is
`throttle "<name>" <max> per <window_ms>`:

```flux
throttle "fetches" 5 per 60000
  web.fetch($url)
```

### debounce

Keyed cross-turn coalescing: each arrival records a last-trigger timestamp for the name in the
session store; the body runs only once `wait_ms` has elapsed since that key's last trigger. The
header is `debounce "<name>" <wait_ms>`:

```flux
debounce "rebuild" 300
  bash("rebuild.sh")
```

---

## Guards and assertions

### assert

Abort the flow if the condition is falsey. The optional second argument is the error
message:

```flux
assert $hits, "grep returned no results"
assert $gate
```

### verify

Run a command and assert its output contains a pattern (substring match). The spelling is
`verify <cmd> contains <expect> [: "message"]` — a sibling of `assert`:

```flux
verify bash("cargo test") contains "test result: ok": "tests failed"
verify bash("echo hi") contains "hi"
```

The optional `: "message"` suffix overrides the default error text.

---

## Pure (no-IO) expressions

### expr — arithmetic and predicates

Native text accepts operator formulas in bind RHS and condition positions. `$name` references are
lowered into the `expr.vars` map, and dotted `$issue.state` becomes lenient dotted access on the
`issue` variable inside the formula:

```flux
$ok = $score >= 0.8
when $issue.state == "opened" && $issue.upvotes > 2
  return true
repeat 10
  until len($queue) == 0
  do poll
```

`format` renders an `expr` natively only when that lowering is invertible: every formula variable
maps directly to the same `$name` (or dotted reads from it). Otherwise — for example a formula
using the expr **function library**, since `round(…)` in text would parse as an op call named
`round` — it keeps the `@json` escape:

```flux
@json {"kind": "bind", "name": "rounded", "value": {"kind": "expr", "formula": "round(price, 2)", "vars": {"price": {"kind": "var", "name": "price"}}}}
```

The call-style spelling below remains **aspirational**: in text, `expr(…)` parses as an ordinary op
call named `expr` (and the `name: $sym` argument form doesn't parse at all).

```flux
# ASPIRATIONAL — does not parse today
$total  = expr("price * qty", price: $price, qty: $qty)
$scaled = expr("round(base * 1.2, 2)", base: $base)
```

Standard precedence: `*` and `/` before `+` and `-`; parentheses for grouping.
Nesting is allowed: `round(max($a, $b) * 1.1, 2)`. All variable names used in
the formula must be declared in `vars` — undeclared identifiers error. The formula language also
supports comparison, boolean, and string functions — see [`reference.md`](reference.md#expr) for
the full whitelist.

| Token | Meaning |
|---|---|
| `+` `-` `*` `/` | arithmetic |
| `(expr)` | grouping |
| `round(x, n)` | round to n decimal places |
| `abs(x)` | absolute value |
| `min(a, b)` | minimum |
| `max(a, b)` | maximum |

### fmt — string interpolation

```flux
$label = fmt("BTC: {price} | 24h: {change}%")
```

Substitutes from already-bound session symbols. Identical semantics to `{sym}` in
string literals, but explicit about being pure.

### jq — JSON path extraction

The native text spelling of the pure `jq` node is the **`$var.path` sugar** (below). The
call-style `jq(".path", $raw)` parses as an ordinary op call named `jq`, **not** the pure node —
bracket paths and non-symbol inputs are written via `@json`:

```flux
$price = $raw.bitcoin.usd     # native sugar for the jq node
```

```flux
@json {"kind": "bind", "name": "first", "value": {"kind": "jq", "path": ".results[0].value", "input": {"kind": "var", "name": "response"}}}
```

Path syntax: a leading `.` followed by dot-separated field names with optional `[n]`
array-index suffixes. This is a strict subset of jq — no filters, pipes, or
conditionals. Allowed forms:

- `.field`
- `.field.nested`
- `.field[0]`
- `.field[0].nested`

**Field-access sugar (P6):** when the input is a plain symbol and the path is a simple dotted field path
(no array index), you may write `$var.path` instead of `jq(".path", $var)`:

```flux
$kind = $plan.kind          # sugar for jq(".kind", $plan)
$txt  = $plan.message.text  # sugar for jq(".message.text", $plan)
```

This is a *bind-value* form: the lowered `jq` node, like any computed value, is only valid as a bind
value — not inline as a `match` subject or call argument (bind it first). Bracket paths (`.items[0]`) and
non-symbol inputs keep the explicit `jq(…)` / `@json` form.

### parse — type coercion

Convert a string result (typically from `jq` or `fmt`) into a typed value. Like `fmt(…)`,
`parse(…)` is special-cased in the expression grammar, so it lowers to the pure node rather than
an op call:

```flux
$price_num = parse($raw.price, as: "f64")
$flag = parse($raw.enabled, as: "bool")
```

`as` is one of `"f64"`, `"i64"`, `"bool"`, `"json"`, `"string"`. Coercion failures error rather
than silently defaulting.

---

## return

```flux
return $hits      # end the flow with a value
return "done"     # literal return value
return            # return null
```

`return` is an **unconditional early exit from the entire flow**. Execution after a
`return` is unreachable. This is consistent with every mainstream language.

To conditionally exit:

```flux
when $done
  return $result
# execution continues here only if $done was falsey
bash("continue working")
```

---

## peek

`peek` reads the current in-session value of a named symbol without IO. Returns the
stored value, or an empty result if the symbol is not yet bound. Useful for resumable flows.

The native spelling is the keyword form `peek $name` — an expression, valid as a bind value or a
condition (`peek(…)` call-style still parses as an ordinary op call named `peek`):

```flux
$prev = peek $last_result

unless peek $survey
  $survey = read("big.log")
```

---

## External references (things)

A `thing` node references an external object. The native spelling is the expression form
`thing <kind> <selector> "<value>"`, valid as a bind value:

```flux
$ticket = thing ticket id "FLUX-42"
$author = thing person name "timo"
$config = thing file path "config.yaml"
$widget = thing custom "widget" key "w-1"
```

Built-in kinds: `context`, `file`, `person`, `ticket`, `email`, `repo`, `dataset`,
`calendar_event`, `url`, `secret`; a custom kind is spelled `thing custom "<name>" …`.
Selector words: `id`, `name`, `path`, `query`, `key`.

The `@kind(key: value)` annotation spelling (`$ticket = @ticket(id: "FLUX-42")`) remains
**aspirational** — in text, `@` introduces only `@json` (anywhere) or `@effect(...)` (before a
bind).

---

## Async, cross-turn state, and durability

### await

Suspend until an external event arrives. The header is `await [$bind[: Type] =] "source"`:

```flux
await $push = "github.push"
await $count: Number = "user_input"
await "webhook"
```

The event source is a string label. The optional type annotation (`as_type`) is coerced leniently
onto the received value; a type annotation requires a binding.

**Implemented (P6a):** a **top-level** `await` suspends the flow for cross-turn resume — the interpreter
records the suspend point (`FlowOutcome.suspension` + a `RunEvent::Awaiting` trace), and the engine
persists it (a `suspensions` table) and resumes via `resume_flow` when the awaited input arrives next
turn; the already-run prefix is **not** re-executed. `await` is **top-level only** in v1 (the analyzer
rejects it nested inside `when`/`repeat`/`each`/… ), and the optimized `execute_plan` path does not suspend.

### checkpoint

A **top-level-only** durable resume marker (like `await`): a later re-run of the same flow in the
same session fast-forwards past the already-completed prefix. The label must be a non-empty
literal:

```flux
checkpoint "phase-1"
```

### once

At-most-once side effects — an effect-level `memo`. The header is `once "<label>" [-> $bind]` + an
indented body; the label is the idempotency key and must be a non-empty literal:

```flux
once "send-welcome"
  send_email($welcome_msg)

once "charge" -> $receipt
  pay()
```

### scope / finally

RAII-style acquire → use → release with guaranteed cleanup. The header is
`scope [$bind = <acquire>]` + an indented body, then a `finally` arm (at the same indent as the
`scope`) + an indented cleanup block that **always** runs:

```flux
scope $h = lock.get("deploy")
  deploy()
finally
  lock.release($h)
```

A bare `scope` (no acquire) still guarantees its `finally` runs. If the acquire errors, the
resource was never taken, so `finally` does not run.

### saga

Compensating transaction: `saga` + repeated `step` arms, each with an indented body and an
optional `undo` arm. On a later step's failure, the registered undos run in reverse order:

```flux
saga
  step
    charge()
  undo
    refund()
  step
    ship()
```

---

## Schema declarations *(aspirational — not implemented)*

**None of this section parses today**: file-scope `type` record/union declarations are a design
target, not shipped syntax (`parse_program` accepts only `agent`/`channel`/`datasource`/`trigger`/
`journey`/`op`/`flow` at file scope). Named types can still be *referenced* in flow headers
(`-> RouteResult`); their definitions come from the registered prelude, not from `.flux` text.

Flux-Lang has a lightweight, structural type system. Types would be declared at file
scope alongside `flow` definitions — same indentation level, separated by blank
lines. They are used to constrain op call arguments, flow parameters and return
values, and model calls (e.g. `intent_extract`).

The design deliberately avoids JSON Schema verbosity. It borrows the structural
shape of TypeScript and the union syntax of GraphQL.

### Record types

```flux
type Slot
  destination: String
  date:        String
  passengers:  Number
  cabin:       String?    # ? = optional field
```

Fields are `name: Type`, one per line, indented 2 spaces. `?` suffix marks an
optional field (may be absent or null). All built-in types are valid field types;
nested record types and `List<T>` are also valid:

```flux
type RouteResult
  intent:    Intent
  slots:     CallerSlots
  response:  String
  escalated: Bool
```

### Union types

```flux
type Intent
  | book_flight
  | change_booking
  | cancel_booking
  | baggage_enquiry
  | escalate_agent
```

Each variant is a `| name` line, indented 2 spaces. Variant names are
`snake_case`. A union value is matched against with `when $x == "variant_name"`
(the runtime represents variants as strings).

### Using types in flows

Types appear in:

- Flow parameters: `flow route-call(utterance: String, caller_id: String) -> RouteResult`
- Op named arguments: `intent_extract($utt, schema: CallerSlots, intents: Intent)`
- Assert messages give richer context: `assert $slots.destination, "no destination in utterance"`

### Model-backed ops and schema

The key use case for schema declarations is constraining model calls. When an op
is declared as model-backed (e.g. `intent_extract`), passing `schema:` and
`intents:` named arguments tells the runtime to request structured output from
the model constrained to those types. The flow author writes routing logic
against typed fields; all prompt engineering lives in the op's registered spec.

```flux
$extract = intent_extract($utterance,
  schema:  CallerSlots,
  intents: Intent
)
```

This is the single LLM-cost step in the flow. Everything else — `when`, `assert`,
`confirm`, `return` — is deterministic execution with no token cost.

---

## Types (built-in)

| Syntax | Meaning |
|---|---|
| `String` | UTF-8 text |
| `Number` | 64-bit float |
| `Bool` | boolean |
| `Any` | top type |
| `List<T>` | homogeneous list |
| `Ticket`, `PushEvent`, … | named / registered type |

Type annotations are optional everywhere. The runtime does not enforce them today;
they are documentation and are preserved in the AST.

---

## Edge cases

| Situation | Behaviour |
|---|---|
| `each` over an empty list | `$collect` bound to `[]`; body never runs |
| `each` with no `-> $collect` | results discarded; body still runs |
| `parallel` with one branch | valid; degenerates to sequential bind |
| `parallel` with zero branches | **parses**; round-trips as an empty block (emptiness is an analyzer concern, not a parse error) |
| `pipe` with one step | valid; equivalent to a bare call |
| `confirm` with no body | valid; pure approval gate (`confirm "Proceed?"` on its own line) |
| `retry` wrapping `confirm` | denial is fatal — not retried |
| Flow with empty body | **parses** to an empty-body flow (emptiness checks are the analyzer's job) |
| `loop` `until` | stop-when-true guard, evaluated **after** each iteration |
| `repeat` `until` | `until` must be the first line of the body; evaluated after each iteration |

---

## Complete examples

Both examples use the shipped spellings only: single-line calls, the single-object named-argument
form, and `branch $name` parallel arms.

### eval-smoke.flux

```flux
flow eval-smoke
  $baseline   = eval_run("mock")
  $sessions   = eval_sessions($baseline)
  $mined      = painpoints_collect($sessions)
  $candidates = improvements_aggregate({mined: $mined, reviewed: []})
  return $candidates
```

### improve.flux (abridged)

```flux
flow improve -> EvalReport
  $baseline = eval_run({adapter: "local", dir: "suites", flux_bin: "target/debug/flux", trials: 3})
  $sessions = eval_sessions($baseline)
  $digest   = sessions_digest($sessions)

  parallel
    branch $mined
      $mined = painpoints_collect($sessions)
    branch $reviewed
      $reviewed = task({role: "reviewer", task: "Review these flux eval sessions for failure modes.\nSessions:\n{digest}\n\nReturn ONLY a JSON array of findings."})

  $candidates = improvements_aggregate({mined: $mined, reviewed: $reviewed})

  repeat 3
    until $done
    $tasks    = task({role: "planner", task: "Turn these candidates into AT MOST 2 tasks:\n{candidates}"})
    $snapshot = git_snapshot()
    change_implement({tasks: $tasks, limit: 2})
    $gate     = gate_check()

    when $gate
      $candidate = eval_run({adapter: "local", dir: "suites", trials: 3})
      when score_compare({baseline: $baseline, candidate: $candidate})
        git_stage(["."])
        git_commit("improve: adopt candidate")
        $baseline = eval_adopt($candidate)
      else
        git_revert($snapshot)
    else
      git_revert($snapshot)

    $done       = candidates_empty($candidates)
    $candidates = candidates_advance($candidates)

  return $baseline
```

---

## Toolchain

- `lexer.rs` / `parser.rs` — the sole lossless, indentation-aware grammar. The parser always returns
  a tolerant CST with recovery diagnostics, which keeps incomplete editor buffers useful.
- `cst_decode.rs` / `lower_cst.rs` / `parse.rs` — recursively lower structured CST declarations,
  statements, blocks, and expressions; strict `parse` rejects recovered errors and returns
  `FlowError::Parse` (never panics). There is no production logical-line or second source parser.
- `format.rs` — `format(ast: &DraftAst) -> String`. Canonical emitter, always 2-space indentation,
  brace-free indentation blocks; emits `@json` for shapes without a native form. Separate from `render.rs`
  (a lossy one-way terminal display tree).

Round-trip invariant: `parse(&format(&ast)) == ast` — native spellings for every node kind,
with unspellable shapes (non-identifier names (L-18), non-invertible `expr`, bracket-path `jq`)
falling back to `@json`; property-tested (`tests/roundtrip_property.rs`).

`flux run <app.flux>` runs a multi-agent program through the `flux-app` host (see
[`../../../docs/designs/flux-lang-evolution.md`](../../../docs/designs/flux-lang-evolution.md) §6); the
`fluxlang compile [FILE]` subcommand (reads stdin when `FILE` is omitted) runs `parse` on Flux-Lang
text and emits the resulting `DraftAst` as JSON.

---

## Relationship to the JSON wire format

| Property | Text (`.flux`) | JSON (wire) |
|---|---|---|
| Who writes it | humans, editors | SDKs, tooling, runtime hosts |
| Where it lives | `examples/`, user repos | API values, authored-flow/session storage |
| Round-trips | via `parse` + `format` | via `serde_json` |
| Comments | yes (`#`) | no |
| Multi-line strings | yes — verbatim `"""…"""` (L-39), auto-emitted for any newline-bearing string | escaped `\n` in JSON string (the wire format has no triple-quote form) |
| Named args | single object argument (`op({k: v})`) | same — one object argument names the params |
| Type annotations | yes (params/returns) | yes (same `TypeRef` serde) |
| String interpolation | `{sym}`, escape `{{` `}}` | same `{sym}` inside JSON strings |

The JSON format remains the authoritative wire format. The text format is a
programmer-facing projection — nothing it can express is absent from the JSON format.
