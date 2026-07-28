# Design: flux-lsp round 2 — from "capability present" to "capability correct"

**Status:** planned 2026-07-29 · **Pillar:** Language · **Epic slug:** `flux-lsp-round-2` ·
**Stories:** [L-85](../stories/L-85-lsp-cursor-aware-completion.md),
[L-86](../stories/L-86-lsp-cst-precise-hover.md),
[L-87](../stories/L-87-lsp-references-and-rename.md),
[L-88](../stories/L-88-lsp-cst-driven-formatter.md),
[L-89](../stories/L-89-lsp-diagnostic-truth.md),
[L-90](../stories/L-90-lsp-parse-cache-and-incrementality.md),
[L-91](../stories/L-91-lsp-module-split-and-protocol-harness.md)

Successor to [flux-lsp.md](flux-lsp.md) (epic closed: L-64…L-70 + L-73, all `done`). That epic
answered *"does `.flux` have editor support at all?"* — the answer is yes, and the binary ships
(`crates/flux-lsp/Cargo.toml:12`, `dist = true`). This epic answers the next question: *"is each
capability actually right?"* — because several of them are advertised at full strength and
implemented at a fraction of it.

## Why

A read of `crates/flux-lsp/src/main.rs` (1800 lines, one file) against what `initialize` advertises
(`main.rs:182-218`) turns up seven concrete gaps. Each is verified against the tree at 0.28.0.

1. **Completion is position-blind.** `completion` (`main.rs:256-261`) takes
   `params.text_document_position` and never reads `.position`; `completions(text)`
   (`main.rs:343-382`) returns the *union* of every registered op, every node kind, every prelude
   type, and every `$symbol` in the buffer, in that order, for every cursor. The original design
   promised "cursor context from the token at the offset (`$`/`@` sigil, statement head, arg
   position)" ([flux-lsp.md:50](flux-lsp.md)) — that was never built. Worse, the `$var` list comes
   from `scan_symbols` (`main.rs:709-731`), a raw byte scan for `$`, so it offers variables bound in
   a *different* flow, variables that appear only inside string literals, and variables not yet in
   scope — while go-to-definition, on the same buffer, is scope-correct (`resolve_var` +
   `better_binding`, `main.rs:1029-1064`). Two answers to the same question, one right.

2. **Hover is textual, not syntactic.** `hover_at` (`main.rs:397-415`) resolves the word under the
   cursor with `word_at` (`main.rs:686-706`), an ASCII scan of the raw line. It cannot tell code
   from a comment or a string literal, so hovering `read` inside `"please read the file"` renders
   the `read` op card. It also never hovers a `$var` — `word_at` stops at the `$` and the lookup
   chain only consults ops, node kinds, and prelude types — even though the CST token lookup
   (`token_at`, `main.rs:1001`) and the whole scope model already exist from L-68.

3. **No references, no rename.** Neither `references` nor `rename`/`prepare_rename` is implemented
   (no handler in the `LanguageServer` impl, `main.rs:181-340`). Rename is the edit operation an
   author reaches for most, and the machinery is already sitting there: `Def`/`DefRole`
   (`main.rs:763-786`), `all_var_defs` (`main.rs:941`), and the shadowing-aware `better_binding`
   (`main.rs:1049`) — the exact scope resolution a correct rename needs.

4. **Formatting is opt-out in two of three cases.** `format_document` (`main.rs:103-131`) returns
   `None` for any multi-declaration module — `Program` groups declarations by kind and cannot
   reproduce source order, so formatting would reorder the author's file (pinned by
   `formatting_is_deliberately_disabled_for_modules`, `main.rs:1591`). And a flow *with comments*
   gets a CST re-indent that canonicalizes indentation only, leaving interior spacing untouched —
   named in the code as "the documented remaining work" (`main.rs:97-102`). So the canonical
   formatter runs only on comment-free single flows, which is the minority of real files. There is
   no range formatting either. The fix for all three is the same: format *from the CST*, not from
   the AST.

5. **The catalog stops at the file edge — and produces false warnings.** `authoring_registry`
   (`main.rs:27-46`) registers built-ins, cognition, datasource, and web; `signatures_for_document`
   (`main.rs:79-91`) adds the composites declared *in the same buffer*. Nothing loads the composite
   ops the host actually installs from disk — `flux_flow::composites::DynamicComposites::load`
   (`crates/flux-flow/src/composites.rs:100`) reads `.flux/flows`, `.flux/ops`, and their global
   twins. A flow that calls a project composite therefore gets an "unknown operation" warning in the
   editor and runs fine in the CLI. Compounding it, *every* analyzer finding is emitted at
   `WARNING` (`lsp_warning`, `main.rs:553-561`) with no `code` — a composite cycle, an unbound
   symbol, and a wrong argument count all render as the same yellow squiggle, and no client can
   filter or code-action on them.

6. **"Incremental" is edit application, not incremental reparse.** `did_change`
   (`main.rs:237-250`) applies ranged edits to the stored `String`, then `refresh`
   (`main.rs:165-177`) calls `parse_cst` on the whole buffer — no rowan node reuse, which is what
   L-70's acceptance actually asked for. And nothing caches the resulting tree: `completion`,
   `hover`, `semantic_tokens_full`, and `formatting` each re-parse the document from text on every
   request (`signatures_for_document:81`, `format_document:107`, `semantic_tokens:1142`), some of
   them twice per call. Semantic tokens are full-document only — `range: Some(false)` and no delta
   (`main.rs:210-211`) — so every keystroke in a client that renders them re-serializes the whole
   token stream.

7. **One file, no protocol-level test.** The crate is `Cargo.toml` + `README.md` + a single
   `src/main.rs`; the original design specified a module shape (`server.rs` / `document.rs` /
   `convert.rs` / `diagnostics.rs` / `completion.rs` / `hover.rs` / `format.rs` / `catalog.rs` —
   [flux-lsp.md:40-43](flux-lsp.md)). Every test calls an internal function directly; the design's
   Verification section asked for a server driven "over an in-memory duplex"
   ([flux-lsp.md:131](flux-lsp.md)), which does not exist — so no test proves that `initialize`
   advertises what the handlers implement, or that a handler is wired at all.

## Approach

**Cash the CST that is already paid for.** Every gap above has the same root cause: features were
built on the cheapest substrate that passed their test (a line scan, a byte scan, an AST round-trip,
a full reparse) while the lossless CST and the L-68 scope model sat next to them. Round 2 moves each
feature onto the tree.

Two stories are foundations and land first:

- **L-90 (parse cache + incrementality)** gives every other handler a cached `Parse` per URI instead
  of a fresh parse per request. It is a prerequisite in the practical sense — cursor-aware
  completion and CST hover parse *more*, not less, and should not pay for it per keystroke.
- **L-85 (completion)** is the highest-visible-value single fix: it is the capability an author
  touches on every line, and today it is the least correct.

The rest are independent and can ship in any order: L-86 (hover), L-87 (references/rename), L-88
(formatter), L-89 (diagnostic truth). **L-91 closes the epic** — the module split is deliberately
*last*, so the file moves once, after the code that will live in those modules exists; it also
carries the in-memory-duplex harness that retrofits protocol-level coverage over the whole surface.

**One rule holds across the epic:** the LSP stays a *reader*. `authoring_registry`'s comment
(`main.rs:32-34`) states the invariant — catalog-only construction, no model, network, or credential
IO at startup. L-89 loads composites from the workspace, which is the epic's only new IO; it goes
through `flux_system::System` like everything else, is read-only, and must not turn editor startup
into a workspace crawl.

## Decisions

- **Scope-correct beats complete.** Where completion and go-to-definition disagree, the scope model
  wins; a completion list that omits an out-of-scope `$var` is correct even though it is shorter.
- **No new advertised capability without a handler test.** L-91's harness makes the pairing
  checkable; `initialize` claiming a capability the server does not answer is a bug of the same
  class as `range: Some(false)` advertised alongside a full-only implementation.
- **Formatting never reorders.** The module opt-out (`main.rs:93-96`) is lifted only by an
  order-preserving CST formatter, never by teaching `Program` a heuristic order.
- **Diagnostic severity follows the analyzer's own judgement**, not a blanket `WARNING`; a finding
  that makes a flow un-runnable is an `ERROR`.
- **Helix stays the reference client** for diagnostics/completion/hover/formatting, and still does
  not render semantic tokens ([flux-lsp.md:104](flux-lsp.md)) — L-90's range/delta work is for VS
  Code and Neovim, and is the lowest priority item in the epic.

## Verification

- Per-story failing-first tests, as usual.
- L-91's in-memory duplex harness is the epic-level gate: a scripted session (`initialize` →
  `didOpen` → `didChange` → completion/hover/references/rename/format/symbols/semantic tokens →
  `shutdown`) asserting that every capability advertised in `initialize` returns a well-formed
  response.
- The full dev-loop gate (`cargo test --workspace`, `clippy -D warnings`, `fmt`, `flux-codegate`)
  stays green; `flux-lsp` remains an L6 leaf with no new upward dependency.
- End-to-end sanity in Helix (`hx examples/*.flux`) with the repo-local
  [`.helix/languages.toml`](../../.helix/languages.toml), as in the first epic.
