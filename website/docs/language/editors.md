---
title: Editor setup
description: Syntax highlighting and language intelligence for .flux files — the full Helix recipe, plus Neovim, Zed, and IntelliJ/TextMate pointers.
---

# Editor setup

Editor support for `.flux` files is two independent pieces, matching how modern editors split
the work:

- **Colour** comes from the tree-sitter grammar,
  [`codewandler/flux-tree-sitter`](https://github.com/codewandler/flux-tree-sitter) —
  highlight/injection/locals queries for tree-sitter-based editors.
- **Everything else** — live diagnostics, completion, hover, formatting — comes from the
  **`flux-lsp`** language server, built from the flux repository.

The two never compete: Helix, for instance, renders colour via tree-sitter *only* (it does not
apply LSP semantic tokens, as of 25.07). So the natural order is **set up highlighting first,
then layer the language server on top** — each section below follows that shape.

## Install the language server

`flux-lsp` is not yet a released binary — install it from source:

```bash
cargo install --git https://github.com/codewandler/flux flux-lsp
```

From a clone of the repository, `cargo install --path crates/flux-lsp` does the same, and
`task install` installs the `flux` CLI and `flux-lsp` together. Verify with `which flux-lsp` —
the editor configs below expect it on `$PATH`.

## What the LSP gives you

| Capability | Details |
|---|---|
| **Diagnostics** | live and error-recovering, with real source spans — parse *and* analysis errors as you type |
| **Completion** | triggers on `$` and `@`; ops, node-kind keywords, prelude types, and in-scope `$vars` |
| **Hover** | op signatures with effects and risk, node-kind docs, prelude-type docs |
| **Formatting** | whole-document formatting via the invertible formatter |

Go-to-definition, document symbols, and semantic tokens are **not implemented yet** — they are
on the roadmap, so don't be surprised when an editor's "goto definition" reports no result.

## Helix

The reference recipe — Helix needs config only, no extension (verified on Helix 25.07.1).

### Syntax highlighting

Add to `~/.config/helix/languages.toml`:

```toml
[[language]]
name = "flux"
scope = "source.flux"
file-types = ["flux"]
comment-token = "#"
indent = { tab-width = 2, unit = "  " }

[[grammar]]
name = "flux"
source = { git = "https://github.com/codewandler/flux-tree-sitter", rev = "main" }
```

> **Tip:** if your Helix came from a distro package, it ships all default grammars prebuilt —
> put `use-grammars = { only = ["flux"] }` at the top of `languages.toml` before the next step
> so Helix doesn't clone and compile ~200 grammars you already have, and remove the line
> afterwards.

Fetch and build the grammar:

```bash
hx --grammar fetch
hx --grammar build
```

Then install the highlight queries, copied from the exact grammar source Helix just fetched (so
queries and compiled grammar can never drift apart):

```bash
mkdir -p ~/.config/helix/runtime/queries/flux
cp ~/.config/helix/runtime/grammars/sources/flux/queries/*.scm ~/.config/helix/runtime/queries/flux/
```

Opening any `.flux` file now shows full colour.

### Add the language server

[Install `flux-lsp`](#install-the-language-server), then extend the same `languages.toml` —
declare the server and reference it from the language:

```toml
[language-server.flux-lsp]
command = "flux-lsp"

[[language]]
name = "flux"
scope = "source.flux"
file-types = ["flux"]
comment-token = "#"
indent = { tab-width = 2, unit = "  " }
language-servers = ["flux-lsp"]
```

### Verify

```bash
hx --health flux
```

should report the tree-sitter parser ✓, highlight queries ✓, and `flux-lsp` found. Open a
`.flux` file and check the live experience: colour everywhere, squiggles on a deliberate typo,
hover on an op name, completion after typing `$`, and `:format`.

Diagnostics, completion, and hover understand multi-declaration modules and the stable cognition,
datasource, and native-web operations provided by the CLI. Formatting currently applies only to a
bare single-flow file; modules are left unchanged so declaration order is never rewritten.

### Working in the flux repo

The repository ships a repo-local
[`.helix/languages.toml`](https://github.com/codewandler/flux/blob/main/.helix/languages.toml)
that Helix merges over your global config, so inside a checkout the wiring above is already
declared — you still need the grammar built and `flux-lsp` on `$PATH` (both per-machine steps).

## Neovim

Highlighting via [nvim-treesitter](https://github.com/nvim-treesitter/nvim-treesitter) —
register the parser and the filetype:

```lua
local parser_config = require("nvim-treesitter.parsers").get_parser_configs()
parser_config.flux = {
  install_info = {
    url = "https://github.com/codewandler/flux-tree-sitter",
    files = { "src/parser.c", "src/scanner.c" },
    branch = "main",
  },
  filetype = "flux",
}
vim.filetype.add({ extension = { flux = "flux" } })
```

Then `:TSInstall flux` and copy the grammar repo's `queries/*.scm` into `queries/flux/` on your
runtimepath.

For language intelligence, `flux-lsp` is a standard stdio server — register it with your LSP
client of choice (for example a custom [lspconfig](https://github.com/neovim/nvim-lspconfig)
server definition with `cmd = { "flux-lsp" }` for the `flux` filetype). No packaged Neovim
config is shipped yet. Unlike Helix, Neovim *can* layer LSP semantic tokens over tree-sitter
colour — `flux-lsp` does not emit them yet, so tree-sitter provides all colour there too.

## Zed

The grammar targets Zed's tree-sitter engine, but there is no tested Zed extension or
step-by-step recipe yet — watch
[`codewandler/flux-tree-sitter`](https://github.com/codewandler/flux-tree-sitter) for progress.

## IntelliJ and TextMate-based editors

[`codewandler/flux-editors`](https://github.com/codewandler/flux-editors) ships the TextMate
grammar (VS Code, Sublime Text, and other TextMate-compatible editors) and a native IntelliJ
plugin for their own ecosystems.

## Related docs

- [Tooling](./tooling.md) — running, previewing, compiling, and formatting flows from the CLI.
- [Flows & syntax](./flows-and-syntax.md) — the grammar you'll be editing.
- [Getting started](../getting-started.md) — install the CLI and run a first flow.
