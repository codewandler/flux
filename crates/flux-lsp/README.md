# flux-lsp

A Language Server (LSP) for Flux-Lang — editor-grade support for `.flux` files, driven by the
lossless CST front-end in `flux-lang`.

## Capabilities

| Capability | Details |
|---|---|
| Diagnostics | live and error-recovering, with real source spans across every flow/composite op in a module |
| Completion | triggers on `$` and `@`; stable host + module-local ops, node-kind keywords, prelude types, in-scope `$vars` |
| Hover | stable host and module-local op signatures with effects/risk, node-kind docs, prelude-type docs |
| Formatting | whole-document formatting via the invertible formatter for bare flows; a commented flow is re-indented CST-first so comments survive (indentation is canonicalized, other spacing is left as-is); modules are left unchanged to preserve declaration order |
| Document symbols | a per-file outline of every `flow`/`op` with its parameters and `$var` binds |
| Go-to-definition | a `$var` use jumps to its binding (with inner-scope shadowing); an op/flow reference jumps to its declaration |
| Semantic tokens | full-document tokens for clients that render them (VS Code, Neovim over tree-sitter), including the distinctions a grammar can't make — a registry-known op vs an unknown identifier, and a `$var` bind vs a use |

Note on highlighting: **Helix does not render LSP semantic tokens** (as of 25.07) — colour there comes
from the [`codewandler/flux-tree-sitter`](https://github.com/codewandler/flux-tree-sitter) grammar.
The semantic-tokens feature targets VS Code / Neovim and the semantic classification a grammar cannot
compute.

Incremental document sync (ranged `didChange` edits) keeps large buffers cheap to update.

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
