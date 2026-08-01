# Flux syntax simplification — one way to write each thing

**Status:** accepted · **Pillar:** Language · **Epic:** [L-102](../stories/L-102-flux-syntax-simplification-epic.md) · **Stories:** [L-103](../stories/L-103-fluxlang-fmt.md) · [L-104](../stories/L-104-canonical-corpus-migration.md) · [L-105](../stories/L-105-single-dialect-syntax-spec.md) · [L-106](../stories/L-106-legacy-spelling-deprecation.md) · [L-107](../stories/L-107-remove-legacy-grammar.md) · [L-108](../stories/L-108-one-assignment-vocabulary.md) · [L-109](../stories/L-109-pure-builtin-namespace.md) · [L-110](../stories/L-110-unify-lit-and-templates.md) · [L-111](../stories/L-111-match-and-when-ergonomics.md) · [L-112](../stories/L-112-syntax-consistency-fixes.md)

**Relation to L-94:** complementary but different axis. The notation workbench adds *projections* of
the AST; this proposal simplifies the *one surface people actually author*. It recommends
deprioritizing L-98/L-99 in favour of this work.

## Diagnosis: the language is simpler than the surface

The canonical dialect that `format(ast)` emits today is already good — bare locals, `op(k: v)`
named inputs, `retry 3, backoff: exponential, delay: 500ms`, `timeout 30s` (verified by running the
formatter over mixed-dialect input). The problem is not the canonical grammar; it is that **the
canonical grammar is one of several dialects the parser accepts, the docs teach, and the corpus
uses** — and nothing pushes an author (human or model) toward the canonical one.

Concretely, the same program can be spelled today with:

| Dimension | Canonical | Also accepted | Where the non-canonical form lives |
| --- | --- | --- | --- |
| Locals | `x = read(…)` | `$x = read(…)` | `agent-loop.flux`, every `examples/*.flux`, most doc snippets |
| Call inputs | `grep(pattern: "x")` | `grep({ pattern: "x" })` | nearly the whole corpus uses the braced form |
| Bare calls | `flaky_step()` | `do flaky_step` | doc examples (`do poll`, `do file_bug $u`) |
| Control headers | `retry 3, backoff: exponential` | `retry 3 backoff exponential delay 500` | pre-L-96 sources |
| Loop guard | `repeat 10, until: done` | `until $done` as first body line | `agent-loop.flux:27` |
| Durations | `timeout 30s`, `delay: 500ms` | bare ms: `timeout 30000` | `syntax.md` P6 section, examples |
| Await guard | `await x = "src", when: cond` | `await x = "src" when cond` | `agent-loop.flux:62` |
| Race deadline | `race 5000` | `race timeout: 5s` alias | spec |
| Interpolation | `"built {sha}"` (implicit in every string) | `fmt("built {sha}")` node | examples pre-bind `fmt` results that plain literals already express |

That is **nine doubled dimensions**; the parser, the formatter, four editor grammars
(Prism, tree-sitter, TextMate, IntelliJ), the LSP, the docs, and every model prior pay for each of
them. The cost is not hypothetical: the pinned tree-sitter grammar cannot parse the *canonical*
dialect at all — 7 of 15 canonical examples fail on bare-identifier binds, typed binds, `ctx`
blocks, `+=`, and `goal` lines (`.github/workflows/tree-sitter-corpus.yml:21-30`), so Helix/Neovim/
Zed users see errors on exactly the spelling the formatter emits. Every doubled dimension doubles
what four grammars must mirror. And the flagship file — `crates/flux-flow/assets/agent-loop.flux`, the loop every user reads
first — is written almost entirely in the *legacy* column, including pure noise like
`$answer = fmt("")` where `answer = ""` parses fine (verified).

On top of the doubled spellings, three unrelated **assignment vocabularies** coexist:

1. `key: value` — named call inputs, header options, object templates
2. `key value` — module-declaration attribute lines (`kind slack`, `bot_token secret "X"`) and the
   `ctx` block's `purpose "…"` / `budget 8000` / `include $a, $b`
3. `x = value` — binds

And several one-off traps:

- `expr(…)`, `jq(…)`, `peek(…)` written call-style silently parse as *op calls* named
  `expr`/`jq`/`peek` — only `fmt(…)`/`parse(…)` are special-cased. The spec itself warns "Beware"
  (`docs/syntax.md` § Implementation status). Three pure nodes, three different native mechanisms
  (special-cased call, keyword form, dotted sugar).
- `{…}` parses as a `lit` when its content is valid JSON and as an `obj` template otherwise — the
  AST node kind of your value depends on whether you quoted your keys. All-literal and empty
  templates are unspellable natively and round-trip via `@json`.
- `budget` means "dispatch cap" as a block and "char budget" inside `ctx` — one keyword, two units.
- `verify cmd contains "x": "message"` uses a colon-suffix message; its sibling `assert` uses a
  comma.
- The formatter alphabetizes named inputs (args live in a sorted map), so `grep(pattern: …, glob: …)`
  formats back as `grep(glob: …, pattern: …)` — author ordering is lost on every round-trip.
- `docs/syntax.md` § Symbols still says the `$` sigil "is mandatory on every symbol reference" while
  § Calls and binds says bare identifiers are canonical — the spec contradicts itself, and
  aspirational sections (`watch`, `type` declarations, `expr(…)` call form, `@kind(…)`) are
  interleaved with implemented ones.
- There is no `fluxlang fmt`. The canonical formatter exists as a library function, but no CLI
  command normalizes a `.flux` file — so the legacy corpus has no migration path and drifts forever.

## Principle

**Simplify by subtraction, not by adding notations.** Every accepted-but-non-canonical spelling is
a standing tax. The language needs exactly one way to write each construct, a mechanical migration
tool, and then the removal of the second way. This follows the repo's own no-fallbacks/clean-cutover
doctrine, applied to grammar.

## Proposals

Ordered by leverage; P1–P4 are corpus/tooling work with no grammar change and could ship this week.

### P1 — `fluxlang fmt`: the missing migration tool

Add `fluxlang fmt [FILE...]` (and `--check`) that parses any accepted dialect and rewrites the file
in canonical form. This is `parse` + `format`, both of which exist; the CLI subcommand does not.
Everything below depends on it — a deprecation without a mechanical fixer is a nag, not a migration.
Prereq: `format` must preserve comments (it operates on `DraftAst`, which drops trivia — the rowan
CST retains it, so a comment-preserving formatter should reformat *from the CST*, not the AST).

### P2 — Migrate the corpus to the canonical dialect

Run P1 over `crates/flux-flow/assets/agent-loop.flux`, `examples/*.flux`, doc snippets, and the
skill examples. Hand-fix the idioms a formatter can't: `$answer = fmt("")` → `answer = ""`,
`$done = fmt("true")` → `done = true`, fmt-pre-binds → inline interpolated literals where the value
is used once. The corpus is what models imitate and users copy; today it teaches the legacy dialect.

### P3 — One interpolation story

Every string literal already interpolates `{sym}` (reference.md § literals). Make that the taught
form and reserve `fmt(…)` for the one case it adds value: making pure-computation intent explicit
in a bind that would otherwise look like a constant. Delete the "bind fmt first, then pass the
symbol" ceremony from examples — a literal argument interpolates in place.

### P4 — Spec hygiene: one dialect in the docs

`docs/syntax.md` documents only the canonical column (one line per construct: "legacy spellings:
see MIGRATION.md"). Move every *aspirational* section into `flux-lang-evolution.md`. Fix the
§ Symbols / § Calls contradiction — the rule is: bare identifiers; `$` is the escape for
keyword-named locals, nothing more. Document the `?` lenient-access suffix (currently only in
reference.md) and `do`'s deprecation.

### P5 — Deprecate, then remove, the legacy grammar

After P1/P2 have shipped and one release has passed: the strict parser (not the tolerant CST) emits
deprecation diagnostics for the legacy column of the table above; the release after that removes
them (breaking ⇒ MINOR per the repo's SemVer rule). Editor grammars shrink accordingly. The tolerant
CST can keep *recognizing* legacy forms for the LSP's "quick-fix: canonicalize" code action.

### P6 — One assignment vocabulary: `key: value` everywhere

Module declarations and `ctx` adopt the same `key: value` spelling used by calls, options, and
templates:

```flux
channel slack
  kind: slack
  bot_token: secret "SLACK_BOT_TOKEN"
  mode: socket

ctx pack, purpose: "repo-health", chars: 8000
  include prior, status, tests
```

Also renames `ctx`'s `budget` to `chars:` (or `tokens:`), removing the `budget`-block collision.
`include`/`exclude` stay structural lines (they carry lists, like `case`/`branch` carry arms).

### P7 — A closed pure-builtin namespace

Reserve the pure names (`fmt`, `parse`, `jq`, `peek`, `expr`, plus the expr function library) in
*expression position*: call-style spelling of any of them lowers to the pure node, never to an op
call. This deletes the "parses as an ordinary op call named `expr`" trap, gives `expr(…)` and
non-symbol-input `jq(…)` native spellings, and shrinks the `@json` escape to genuinely pathological
shapes (non-identifier names). Op-catalog names are already dotted or domain-specific; a collision
audit is one grep.

### P8 — Unify `lit` and value templates

`{…}` / `[…]` always parse to the template node; lowering normalizes an all-literal template to a
`lit` (or the runtime treats them identically). The author-visible rule becomes "braces are a
value" — no JSON-validity sniffing, and all-literal/empty templates get a native spelling. Wire
compat: decode keeps accepting both node kinds.

### P9 — Ergonomics of the hot path: `match` and `when`

- `match step.kind` — allow dotted/expression subjects by auto-binding internally. Removes the
  ubiquitous `$kind = $step.kind` pre-bind line (see agent-loop.flux, twice).
- `else when <cond>` chaining (spelled like the existing `else` arm). Today the only chain is
  nesting, which real flows (agent-loop's batch arm) show going three deep.
- `case "a", "b"` multi-value arms — two spellings of the same body today require duplication
  (agent-loop's `"chat"`/`"error"` arms are byte-identical).

### P10 — Small consistency fixes

- `verify … contains "x", "message"` — comma message, matching `assert`.
- Durations everywhere; bare-ms numbers in time positions deprecated with the rest of P5.
- Preserve author order of named inputs (ordered map in `Call.inputs`) so `fmt` stops reordering
  arguments; or explicitly bless alphabetical order and let `fluxlang fmt` apply it — either way,
  decide it rather than inherit it from `BTreeMap`.

## What this recommends about the existing epic

- **L-95/L-97 (Railflux out, Glyph): keep** — shipped, useful, cheap.
- **L-98 (Tape) / L-99 (S-Flux): deprioritize or drop.** No consumer is named, and L-97's own
  retrospective shows the compact-notation premise straining on real flows (the production
  agent-loop collapses into a single multi-KB `@{…}` escape line). Two more notations multiply the
  mirror/maintenance surface this proposal exists to shrink.
- **L-100 (Railflux reader): keep deferred** as designed.

## Sequencing

P1 → P2+P3+P4 (one release: "canonical dialect everywhere, tool to get there") → P5 (deprecate) →
P6/P7/P8 (each its own story, each with the golden-regeneration + editor-mirror checklist from
`crates/flux-lang/AGENTS.md`) → P9/P10 opportunistically. P1–P4 are non-breaking; P5–P8 are each
breaking ⇒ MINOR.
