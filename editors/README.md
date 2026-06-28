# Editor support for Flux-Lang

Tooling for authoring `.flux` files (the human-writable Flux-Lang text form). Two artifacts, low → high
effort:

| Path | What | Use it when |
|---|---|---|
| [`textmate/`](textmate/) | **Tier 0** — a TextMate / VS Code grammar. Zero build. | You want color *right now* in IntelliJ (import as a TextMate bundle) or VS Code. |
| [`intellij/`](intellij/) | **Tier 1** — a real IntelliJ plugin: file type + icon, lexer-based highlighting, `#` commenting, brace matching, a color settings page. | You want a shippable plugin in any JetBrains IDE. |

Both highlight the same language; the IntelliJ plugin adds proper file-type identity, an icon, comment
toggling (Ctrl-/), brace matching, and recolorable token kinds. Neither does semantic analysis yet — see
the roadmap (Tiers 2–6: validation, completion, plan preview, a Rust LSP) in the plan that produced this.

> Note: some `.flux` files are the **JSON wire form** (model-emitted plans, e.g. `examples/*.flux`), not the
> text form. Those still open fine but highlight as generic tokens — the grammars target the human text form
> documented in [`../crates/flux-lang/docs/syntax.md`](../crates/flux-lang/docs/syntax.md).
