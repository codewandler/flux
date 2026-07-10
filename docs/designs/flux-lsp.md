# flux-lsp — a Language Server for Flux-Lang, wired into Helix

**Status:** proposed 2026-07-09 · **Pillar:** Language · **Epic slug:** `flux-lsp` (workstream 2 of 2)

A standalone `flux-lsp` server that brings editor-grade language support to `.flux` files —
diagnostics, completion, hover, formatting, and later go-to-definition and semantic-token
highlighting — wired into Helix (`hx`) with config only, no editor extension. Builds on the lossless
CST front-end designed in [flux-lang-cst.md](flux-lang-cst.md).

## Why

Today the only `.flux` surfaces are `flux flow run` and the dev `fluxlang compile`; the IntelliJ
plugin (`~/projects/flux-editors/intellij/`) is a native-PSI plugin that shells out to the CLI and
regex-guesses error lines. There is no LSP. Helix, the target editor, is a config-only LSP client —
point it at a `flux-lsp` binary and every `.flux` buffer gets live language support. The CST
front-end makes this cheap: spans and error-recovery are structural, so diagnostics carry precise
ranges, hover hit-testing is a token lookup, completion has real cursor context, and semantic tokens
fall out of the token stream.

## Crate

`crates/flux-lsp` — a **layer L6** surface binary `flux-lsp` (alongside `flux-cli`). Every dependency
is downward, so the `flux-codegate` layering lint is satisfied:

- `flux-lang` (L0) — `parse`/tolerant-parse, `analyze`, `format`, `schema::node_kind_rows`,
  `prelude::prelude_type_rows`, and the CST.
- `flux-flow` (L3) — `registry::OpRegistry::{op_names, signatures, get}` for op completion/hover.
- `flux-tools` (L2) — `register_builtins` to fill the registry.
- `flux-cognition` + `flux-provider` (L3/L1) — model-op specs backed by a non-generating
  `NullProvider`; the editor never invokes a model.
- `flux-capabilities` + `flux-web` (L5) — datasource and native-web specs backed by an empty
  in-memory index and catalog-only web options; constructing the catalog performs no IO.
- `flux-runtime` (L2) — `ToolRegistry`.
- `tower-lsp` + `lsp-types` + `tokio` — the async LSP server framework (flux already uses tokio).

Registration: root `Cargo.toml` `[workspace] members` + `[workspace.dependencies]`; the **L6 arm** of
`layer()` in `crates/flux-codegate/src/lib.rs:44` (the lint fails an unclassified crate); the
`docs/architecture.md` layer tables.

Module shape: `main.rs` (stdio bootstrap) · `server.rs` (tower-lsp `LanguageServer` impl +
capabilities) · `document.rs` (open-buffer store + line-index) · `convert.rs` (CST `TextRange` ↔ LSP
`Range` via the line-index) · `diagnostics.rs` · `completion.rs` · `hover.rs` · `format.rs` ·
`catalog.rs` (builds the `OpRegistry` once; caches op/node/prelude data).

## Feature map — what each reuses

| LSP feature | Source (reused) | Notes |
|---|---|---|
| **Diagnostics** | tolerant parse (`ERROR` nodes) + `lower` + declaration-local `cst_to_module` range maps | every top-level flow/composite op is analyzed on `didOpen`/`didChange` |
| **Completion** | stable host + module-local op signatures + `schema::node_kind_rows()` + `prelude::prelude_type_rows()` + in-scope `$vars` | cursor context from the token at the offset (`$`/`@` sigil, statement head, arg position) |
| **Hover** | token-at-offset → `OpSignature` (op) / node-kind doc (keyword) / prelude doc (type) | hit-testing trivial on a CST |
| **Formatting** | `format::format` on a bare-flow `DraftAst` | modules return no edit until declaration order is preserved; a future `flux fmt [--check]` remains L-67 |
| **Document symbols** | CST scope model | later (L-68) |
| **Go-to-definition** | `$var` bind sites with def ranges (CST) | later (L-68) |
| **Semantic tokens** | CST token stream classified by `SyntaxKind` | later (L-69); only for clients that render them — Helix does not (see Highlighting below) |

### Authoring catalog and modules

The LSP builds a **catalog-only** registry for stable operations that the CLI host installs: core
tools, cognition, datasource retrieval, and native web. Provider-backed cognition uses
`NullProvider`; datasource retrieval uses an empty `MemoryBackend`; native web uses
`WebOptions::default`. The server reads only their `ToolSpec`s, so editor startup performs no model,
network, filesystem, or credential IO. Dynamically discovered plugin and endpoint operations remain
host-dependent and are not silently accepted.

The CST structures every top-level `flow` and composite `op` declaration. Whole-module lowering
keeps one analyzer range map per declaration, and the LSP analyzes every flow/op against a catalog
containing all module-local composites (including forward references and `expose false` internal
ops). Duplicate/conflicting ops, recursion, bad arguments/types, and unbound symbols stay errors at
their declaration-local spans. Completion and hover include local composites too.

Formatting intentionally remains bare-flow-only. `Program` groups declarations by kind and does not
retain their original cross-kind order, so formatting a module today could reorder source. The LSP
returns no formatting edit for a multi-declaration module until an order-preserving representation
exists.

## Helix wiring (config-only; Helix 25.07.1 present)

No editor extension — Helix is a built-in LSP client. The user-facing recipe (highlighting via
the tree-sitter grammar, then the server on top) lives on the public site at
`website/docs/language/editors.md` (L-73); this section stays the architectural record. Repo-local
`/home/timo/projects/flux/.helix/languages.toml` (also documented for `~/.config/helix/`):

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

## Highlighting & tree-sitter

**Superseded by a sibling repo (2026-07-09):** a native tree-sitter grammar now exists at
[`codewandler/flux-tree-sitter`](https://github.com/codewandler/flux-tree-sitter) (grammar +
highlight/injection/locals queries, Rust/Node bindings, corpus tests) and is the highlighting
story for Helix, Neovim, and Zed. Two corrections to this design's original premise:

- **Helix does not render LSP semantic tokens at all** (as of 25.07) — colour in Helix comes
  *only* from a tree-sitter grammar, so semantic tokens were never a Helix highlighting path.
  This server contributes diagnostics/completion/hover/formatting; the grammar contributes colour.
- **L-69 (semantic tokens) is therefore re-scoped**: still near-free from the CST token stream,
  but only valuable for clients that render them (VS Code, Neovim layered over tree-sitter) and
  for *semantic* distinctions a grammar can't make (e.g. registry-known op vs unknown identifier).

The in-repo Prism (website) and TextMate/IntelliJ (`codewandler/flux-editors`) grammars remain the
highlighting story for their own contexts.

## Prior art (reference only)

The IntelliJ plugin's `FluxVocabulary.kt` (curated keywords/types/ops/annotations + a per-keyword
hover `DOCS` map) is portable *reference data* for completion/hover copy — but it is hand-maintained;
the authoritative sources are the Rust SSOT catalogs (`schema::node_kind_rows`,
`prelude::prelude_type_rows`, `OpRegistry`). The plugin's diagnostic pattern (shell out, non-zero
exit → error) is the model `flux-lsp` improves on with real positioned diagnostics.

## Stories

L-64 (crate skeleton + text sync + diagnostics + Helix wiring) → L-65 (completion) → L-66 (hover) →
L-67 (formatting + `flux fmt`). Later: L-68 (document symbols + go-to-definition), L-69 (semantic
tokens), L-70 (incremental reparse + comment-preserving format + docs/packaging + epic close).
Depends on the CST foundation (L-57–L-59) in [flux-lang-cst.md](flux-lang-cst.md).

## Verification

- Integration: drive the server over an in-memory duplex — `didOpen` a bad buffer → positioned
  `publishDiagnostics`; completion at a cursor returns the expected op/keyword/`$var` set; hover over
  an op returns its signature; formatting returns canonical text.
- Module integration: several flows plus forward-referenced composite ops analyze independently;
  body diagnostics resolve within the owning declaration; stable cognition/datasource/web ops do not
  produce false unknown-op warnings; a genuinely unknown op still does.
- End-to-end: `hx examples/*.flux` → squiggle at the right span, completion popup, hover card,
  `:format`.
