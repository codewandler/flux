# Flux-Lang CST front-end + native-syntax coverage

**Status:** implemented 2026-07-14 (CST sole accepting parser in L-80) · **Pillar:** Language ·
**Epic slug:** `flux-lsp` (workstream 1 of 2)

Replaces Flux-Lang's hand-written recursive-descent front-end with a **lossless concrete syntax
tree (CST)** built on [`rowan`](https://crates.io/crates/rowan) — the rust-analyzer model — so that
spans and error-recovery are *structural*, not bolted on. In the same pass it closes the **`@json`
syntax gap**: the 16 `Node` kinds that today only round-trip through the `@json` escape get real
native text. Sibling design: [flux-lsp.md](flux-lsp.md) (the editor/LSP workstream that consumes
this).

## Why

Investigation (2026-07-09) confirmed the parser *code* is clean — 3,956 lines, 56 tests, the
`parse(&format(&ast)) == ast` invariant pinned, zero HACK/ambiguity markers. What is missing is an
**error model fit for an editor**:

- **All-or-nothing.** `parse`/`parse_program` return one `FlowError::Parse` on the first error and no
  AST. A live buffer (usually syntactically incomplete while typing) goes dark.
- **No spans.** `Node` (`ast.rs:363`) carries no offsets; parse errors bake a bare `line N:` into a
  string (`parse.rs:84`); `analyze::Diagnostic` is `{ message }` with an AST node-path
  (`analyze.rs:16`). Every "map to an LSP range" feature is blocked.

A retrofit (spans + recovery on the current parser) was considered and rejected in favour of the CST
because the CST makes spans/recovery structural and additionally unlocks semantic-token highlighting,
trivial hover hit-testing, incremental reparse, and a future comment-preserving formatter — the
editor features we actually want. See [flux-lsp.md](flux-lsp.md) for the feature payoff.

## The containment bet — rebuild the front-end only

The existing **semantic AST** (`ast.rs::Node` / `DraftAst`) and everything downstream — `analyze` /
`lower`, `format`, the runtime, the optimizer, the planner emission, and the in-flight
`data-transforms` ops — stay **unchanged**. The CST is a new front-end that lowers *into* today's
`DraftAst`:

```
source ─▶ lexer (lossless, layout-aware) ─▶ tolerant parser ─▶ rowan green tree (CST: spans + ERROR)
                                                                     │
                                          ┌──────────────────────────┴───────────────┐
                                          ▼                                            ▼
                                 cst_to_draft(&SyntaxNode)                     LSP consumes CST directly
                                 → Result<DraftAst, Vec<Diagnostic>>           (diagnostics, hover, completion,
                                          │                                     semantic tokens)
                                          ▼
              UNCHANGED: analyze / lower / format / runtime / optimizer / planner / data-transforms ops
```

The 56 tests + the round-trip invariant are the acceptance backbone that proves the swap is
behaviour-preserving.

## Components

- **`SyntaxKind`** (`crates/flux-lang/src/syntax.rs`, new) — one `#[repr(u16)]` enum covering every
  token kind *and* every node kind, plus `ERROR`, `TRIVIA` (whitespace/comment), and the layout
  tokens below. This is the rowan `Language` alphabet.
- **Lossless layout-aware lexer** (`src/lexer.rs`) — emits a flat token stream that preserves
  *everything*: comments and newlines become **trivia tokens**, a `"""…"""` block is one STRING
  token (the `"""`→escaped-JSON re-encode happens while decoding a structured CST leaf, out of the
  lexer), and significant **`NEWLINE` / `INDENT` / `DEDENT`** tokens carry the
  indentation grammar into the flat model. Tabs-in-indent stays an error (parity). Invariant:
  concatenating all token texts reproduces the source byte-for-byte.
- **Tolerant parser + green-tree builder** (`src/parser.rs`) — a hand-written recursive
  descent that emits `start-node` / `token` / `finish-node` / `error` events and **always completes a
  tree**, wrapping unexpected input in `ERROR` nodes and resyncing at the next `NEWLINE` / `DEDENT`.
- **`cst_to_draft`** (`src/lower_cst.rs`) — strictly projects a clean CST to today's `DraftAst`,
  reproducing it exactly. `src/cst_decode.rs` recursively traverses structured declaration,
  statement, block, and expression nodes; only scalar leaves decode their lossless token text. It
  does not reconstruct logical lines or parse the source a second time. `lower_cst` also records a
  `DraftAst` node-path → `TextRange` **side-map** so the message-only `analyze::Diagnostic` resolves
  to a real LSP range without changing `Node`. Triple-string decoding lives in the scalar-leaf
  helpers.
- **Re-pointed entry points** — `parse`/`parse_program` become `lex → parse → (strict) cst_to_draft`
  (error if any `ERROR` node), so every existing caller (CLI, engine, data-transforms) is unchanged.
  The LSP calls the tolerant path directly and reads the CST + diagnostics.

The first L-59 landing retained the old source parser as the semantic authority while compatibility
settled. L-80 completed the cutover: the CST now owns all production acceptance, strict entry points
refuse its recovered errors/ERROR tokens before structured lowering, and production no longer has a
logical-line projection or line-oriented semantic parser. Frozen pre-cutover source/AST fixtures and
error classifications remain as plain test data, giving compatibility coverage without preserving
executable legacy parser code or a possible runtime bypass.

New L0 deps: `rowan` + `text-size` — pure, no-IO data-structure crates, L0-safe, but new external
deps in the foundational crate (called out in review/CHANGELOG).

## Native-syntax coverage — closing the `@json` gap

Today `format` emits `@json <compact-json>` for any `Node` kind without a native spelling and `parse`
reads it back. Of 43 kinds, **27 are native and 16 are `@json`-only**. In this pass all 16 get native
text. After this, `@json` remains only as the escape for *guarded pathological shapes* of otherwise
native nodes (unspellable names, all-literal `obj`/`list`, bracket-path `jq`) — no *kind* is
`@json`-only.

### Proposed surfaces (⚠ reviewable — these are language design, not just parser mechanics)

Designed to rhyme with the existing native forms (`match`/`case`, `route`/`case`, `fallback`/`branch`,
`parallel`/`branch`, `retry`, `loop`, `timeout`, `budget`, `with_tools`, `ctx`). `[…]` = optional.

**Durability / idempotency (single-header) — L-60**

| Node | Fields | Proposed native text |
|---|---|---|
| `Memo` | name, value, ty?, effect? | `memo $x[: T] = <expr>` (optional `@effect(tag)` line above, like `bind`) |
| `Once` | label, body, bind? | `once "label" [-> $bind]` + indented body |
| `Checkpoint` | label | `checkpoint "label"` (top-level one-liner) |
| `Await` | binding?, source, as_type? | `await [$b[: T] =] "source"` (e.g. `await $reply = "user_input"`) |

**Guard-rails + expr sugar — L-61**

| Node | Fields | Proposed native text |
|---|---|---|
| `Confirm` | message, risk?, body | `confirm "message" [risk high]` + indented body |
| `Throttle` | name, max, window_ms, body | `throttle "name" <max> per <window_ms>` + body |
| `Debounce` | name, wait_ms, body | `debounce "name" <wait_ms>` + body |
| `Verify` | cmd, expect, message? | `verify <cmd> contains <expect> [: "message"]` (sibling of `assert`) |
| `Peek` | name | `peek $name` (inline expression sugar) |
| `Parse` | value, as_type | `parse(<value>, as: "f64")` — special-case `parse(` in the expr parser exactly like `fmt(` (`parse.rs:1990`), so it does not lower to a `Call` to op `parse` |

**Arm/body control-flow (reuse match/case/branch machinery) — L-62**

| Node | Fields | Proposed native text |
|---|---|---|
| `Try` | body, catch?, handler | `try` + body, then `catch [$err]` + handler (two indented blocks) |
| `Race` | timeout_ms, branches, bind? | `race <timeout_ms> [-> $bind]` + `branch $name` arms (twin of `parallel`) |
| `Scope` | acquire?, bind?, body, finally | `scope [$res = <acquire>]` + body, then `finally` + cleanup block |
| `Saga` | steps (body + undo) | `saga` + repeated `step` … `undo` arm pairs |
| `Pipe` | steps, bind? | `pipe [-> $bind]` + indented call steps (native `\|>` operator stays deferred) |

**Structural (heaviest; cleanly deferrable) — L-63**

| Node | Fields | Proposed native text |
|---|---|---|
| `Thing` | thing: `ThingRef { kind, selector }` | `thing <kind> "<selector>"` (e.g. `thing file "src/x.rs"`, `thing url "https://…"`, `thing id "PR-123"`); confirm the exact `ThingRef` kind-enum/selector variants at implementation |

Each coverage story does: **design the surface → CST production → `format` arm → `cst_to_draft`
lowering → round-trip test asserting the node no longer emits `@json`**.

### Guard tests to migrate

`json_fallback_round_trips_statement_and_inline` (`parse.rs:2941`) and
`unsupported_node_uses_json_fallback` (`format.rs:971`) currently assert the fallback using `Once`
and `Thing`. As those go native, the tests switch to a degenerate-shape `@json` example (an
unspellable bind name, or an all-literal `obj`) so they still prove `@json` works as the escape.

## Honest tradeoffs

- **Wider change surface (permanent).** A node kind now touches `SyntaxKind` + tolerant parser +
  `cst_to_draft` + `format` + `DraftAst`. Closing the gap pays this for 16 nodes now; L-51
  (native-expr conditions) and all future syntax work inherit it. This is the real ongoing price of
  the rust-analyzer model on a small, machine-generated DSL — the LSP quality and full native syntax
  are what buy it.
- **Strict-path error-string reproduction.** Several tests pin located error classes
  (`parse_errors_carry_line_numbers`, `"tabs are not allowed for indentation"`, `"the `flow` header
  must start at column 0"`). Lowering must reproduce these strings or the pinned tests are
  *consciously* updated. Inviolable: the round-trip invariant and `DraftAst` shape; negotiable: error
  wording.
- **Two trees.** The CST and `DraftAst` coexist by design (containment). A future, larger step could
  make the CST the sole tree with the semantic AST as a typed view — explicitly **not** in scope here.

## Historical isolation gate

This work **replaced `parse.rs`/`format.rs`'s front-end** and would have collided with the active
`data-transforms` session (L-51 edits the same files). **No worktree.** The stories are hard-gated on
flux-lang's front-end being quiescent, then land in-place on `main`. Confirm before starting L-57
(`git log`/`status` on `crates/flux-lang/src/{parse,format}.rs`, no active L-51).

## Stories

L-57 (SyntaxKind + lexer) → L-58 (tolerant parser + rowan + ERROR) → L-59 (typed layer +
`cst_to_draft` backbone) → L-60..L-63 (native-syntax coverage) → L-80 (sole-parser cutover). See the
board and [flux-lsp.md](flux-lsp.md) for the editor stories (L-64+).

## References

- Node inventory: `crates/flux-lang/src/ast.rs:363` (`enum Node`), `schema.rs:25`
  (`node_kind_rows()` — reflectively derived, so the enum is the SSOT).
- Current front-end: `lexer.rs` (lossless layout tokens), `parser.rs` (tolerant CST), `lower_cst.rs`
  (strict diagnostics + ranges), `cst_decode.rs` (structured CST → semantic AST), `parse.rs` (strict
  public wrappers plus frozen compatibility fixtures), and `format.rs` (canonical AST text).
- Related: [flux-lang-evolution.md](flux-lang-evolution.md) (node-vs-op philosophy, deferred items).
