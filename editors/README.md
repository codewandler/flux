# Editor support for Flux-Lang

Tooling for authoring `.flux` files (the human-writable Flux-Lang text form). Two artifacts:

| Path | What | Use it when |
|---|---|---|
| [`textmate/`](textmate/) | A TextMate / VS Code grammar. Zero build. | You want color *right now* in IntelliJ (import as a TextMate bundle) or VS Code. |
| [`intellij/`](intellij/) | A full IntelliJ plugin for any JetBrains IDE. | You want the complete authoring experience in IntelliJ / RustRover / CLion. |

The **IntelliJ plugin** is the full editor experience (see [`intellij/README.md`](intellij/README.md) for
the complete list and build/run steps):

- **Syntax highlighting** — keywords, `$symbols`, strings + `{interpolation}`, numbers, `@annotations`,
  types, `#` comments; plus a recolorable **color settings page**.
- **Live validation** — reuses the Rust `fluxlang` (`compile` for text, `render` for the JSON wire form)
  to flag parse errors as red squiggles.
- **Completion** — keywords, types, built-in ops, `@annotations`, and the `$symbols` bound in the file.
- **Live templates** — `flow`, `when`, `repeat`, `each`, `match`, `try`, `confirm`, `parallel`, `ctx`, …
- **Quick documentation** (Ctrl-Q) on keywords/nodes.
- **Folding** of indentation blocks and a **structure view** (Alt-7) of `flow`/`type` declarations.
- **Preview Plan (Tree) / Show AST (JSON)** actions that run `fluxlang` on the current file.
- **Line commenting** (`#`, Ctrl-/) and **brace matching** (`()`/`[]`/`{}`).

The **TextMate grammar** is highlighting only (regex scopes, no validation) — the fastest path to color
in any editor, and reusable as the highlighting source elsewhere.

Architecture: a thin Kotlin layer hosts the IDE features over a flat PSI; the real language semantics
(validation, plan preview) are reused from the Rust `fluxlang` toolchain, so they never drift.

> Note: some `.flux` files are the **JSON wire form** (model-emitted plans, e.g. `examples/*.flux`), not the
> text form. Those still open fine but highlight as generic tokens — the grammars target the human text form
> documented in [`../crates/flux-lang/docs/syntax.md`](../crates/flux-lang/docs/syntax.md).
