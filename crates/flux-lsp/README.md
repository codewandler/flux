# flux-lsp

A Language Server (LSP) for Flux-Lang — editor-grade support for `.flux` files, driven by the
lossless CST front-end in `flux-lang`.

## Capabilities

| Capability | Details |
|---|---|
| Diagnostics | live and error-recovering, with real source spans (full-document sync) |
| Completion | triggers on `$` and `@`; ops, node-kind keywords, prelude types, in-scope `$vars` |
| Hover | op signatures with effects/risk, node-kind docs, prelude-type docs |
| Formatting | whole-document formatting via the invertible formatter |

Go-to-definition, document symbols, and semantic tokens are not implemented yet (stories L-68 /
L-69).

## Install

Not yet a release artifact (`dist = false` — shipping binaries is part of the packaging story,
L-70). Build from source:

```bash
cargo install --git https://github.com/codewandler/flux flux-lsp
# or, from a clone:
cargo install --path crates/flux-lsp
# or: task install   (installs the flux CLI and flux-lsp together)
```

## Editor wiring

The server speaks LSP over stdio; any LSP-capable editor can run it. The public per-editor guide
(Helix first — highlighting via the tree-sitter grammar, then this server on top) is
[Editor setup](https://codewandler.github.io/flux/docs/language/editors); inside this repository
the wiring ships as [`.helix/languages.toml`](../../.helix/languages.toml). Design and roadmap:
[`docs/designs/flux-lsp.md`](../../docs/designs/flux-lsp.md).
