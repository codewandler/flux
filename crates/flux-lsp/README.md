# flux-lsp

A Language Server (LSP) for Flux-Lang — editor-grade support for `.flux` files, driven by the
lossless CST front-end in `flux-lang`.

## Capabilities

| Capability | Details |
|---|---|
| Diagnostics | live and error-recovering, with real source spans across every flow/composite op in a module. Composite ops defined in the workspace flow home (`.flux/flows`, `.flux/ops`, and the global roots) are known, so calling one is not reported as unknown. Findings that make a declaration un-runnable are `ERROR`, advisory ones `WARNING`, and every diagnostic carries a stable `code` |
| Completion | cursor-aware: after `$` only in-scope variables, after `@` annotations, at a statement head node-kind keywords and ops, in an argument position ops + `$vars` + prelude types. Nothing is offered inside a comment or a string. Variables come from the scope model, so an inner shadowing bind wins and another flow's binds are never offered. Op items carry their rendered signature and a parameter snippet |
| Hover | resolves the CST token at the cursor, so a word inside a comment or a string does not hover. A `$var` renders its binding — role, owning declaration, bind site — and ops, node kinds and prelude types render their docs. The response carries the token's range |
| Formatting | whole-document formatting driven from the CST, so comments and declaration order are structural facts: a commented flow reaches full canonical spacing with every comment preserved, and a multi-declaration module formats while keeping its source declaration order. The safety net holds — the result must reparse clean, lower to the same module, and keep the same comment multiset, or no edit is produced |
| Range formatting | a selection covering whole statements formats on its own; a partial selection is widened to whole lines |
| Document symbols | a per-file outline of every `flow`/`op` with its parameters and `$var` binds |
| Go-to-definition | a `$var` use jumps to its binding (with inner-scope shadowing); an op/flow reference jumps to its declaration |
| References | every use that resolves to *that* binding — never a same-named variable in another declaration or scope; on a flow/op name, its declaration and every call site |
| Rename | `prepareRename` refuses a position that is not a renameable symbol rather than returning a bogus range; `rename` returns a `WorkspaceEdit` covering exactly the reference set, handling the `$` sigil and validating the new name |
| Semantic tokens | full, delta, and range, for clients that render them (VS Code, Neovim over tree-sitter), including the distinctions a grammar can't make — a registry-known op vs an unknown identifier, and a `$var` bind vs a use |

Note on highlighting: **Helix does not render LSP semantic tokens** (as of 25.07) — colour there comes
from the [`codewandler/flux-tree-sitter`](https://github.com/codewandler/flux-tree-sitter) grammar.
The semantic-tokens feature targets VS Code / Neovim and the semantic classification a grammar cannot
compute.

Incremental document sync (ranged `didChange` edits) keeps large buffers cheap to update. The parsed
tree is cached per document and every handler reads it, so an edit costs one parse no matter how many
requests follow it — on a 2,100-line buffer that is ~12.8 ms per edit-and-query cycle instead of the
~38 ms three separate parses used to cost.

## Install

`flux-lsp` ships as a release binary alongside the `flux` CLI (built for every release target), so
editors can install it without a Rust toolchain — see the [release page][releases]. To build from
source instead:

```bash
cargo install --git https://github.com/codewandler/flux flux-lsp
# or, from a clone:
cargo install --path crates/flux-lsp
# or: task install   (installs the flux CLI and flux-lsp together)
```

[releases]: https://github.com/codewandler/flux/releases

## Editor wiring

The server speaks LSP over stdio; any LSP-capable editor can run it. The public per-editor guide
(Helix first — highlighting via the tree-sitter grammar, then this server on top) is
[Editor setup](https://codewandler.github.io/flux/docs/language/editors); inside this repository
the wiring ships as [`.helix/languages.toml`](../../.helix/languages.toml). Design and roadmap:
[`docs/designs/flux-lsp.md`](../../docs/designs/flux-lsp.md).
