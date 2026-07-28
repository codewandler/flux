---
id: L-86
title: CST-precise hover — including `$vars`, excluding comments and strings
pillar: Language
status: done
epic: flux-lsp-round-2
design: docs/designs/flux-lsp-round-2.md
note: hover_at resolves the word with a raw line scan (word_at, main.rs:686), so `read` inside a string or comment renders the op card, and a `$var` never hovers at all — while the CST token lookup (token_at:1001) and the L-68 scope model already exist
---

# CST-precise hover — including `$vars`, excluding comments and strings

## Goal

Hover answers about the thing under the cursor, not about a word that happens to spell an op name —
and covers the identifier authors ask about most, the `$var`.

## Why (evidence)

- `hover_at` (`crates/flux-lsp/src/main.rs:397-415`) calls `word_at` (`main.rs:686-706`), which
  takes `text.lines().nth(pos.line)` and walks ASCII word characters. It has no syntactic context:
  a comment `# read the config` or a string `"please read it"` hovers the `read` op card.
- The lookup chain consults ops, then node kinds, then prelude types. `$vars` are unreachable —
  `word_at`'s word set is `[A-Za-z0-9_]`, so the `$` is dropped and no branch looks up a binding.
- `Hover.range` is always `None` (`markdown_hover`, `main.rs:748-756`), so clients cannot highlight
  the hovered span.
- The precise substrate exists: `token_at` (`main.rs:1001`) hit-tests the CST, `resolve_var`
  (`main.rs:1029`) resolves a use to its shadowing-correct bind, and `Def` carries `role`
  (`main.rs:763-786`).

## Acceptance

- [x] Hover resolves via the CST token at the offset, not `word_at`; a token inside a comment or a
      string literal produces no hover.
- [x] Hovering a `$var` use renders its binding: role (param / bind), the declaration it belongs to,
      and the bind-site line; hovering the bind itself renders the same card.
- [x] The returned `Hover` carries the token's `range`.
- [x] Op / node-kind / prelude-type hovers keep their current content (`render_op`, `main.rs:733`).
- [x] Failing-first tests: (a) hovering `read` inside a `#` comment and inside a string returns
      `None`; (b) hovering a `$var` use returns a card naming its bind site; (c) an op hover still
      renders its signature with effects/risk.

## Progress
- **Done (2026-07-28).** `hover.rs` resolves through the CST token at the offset rather than the old
  raw-line `word_at` scan, so `read` inside a `#` comment or a string no longer renders an op card. A
  `$var` use renders its binding — role (param vs bind), owning declaration, and bind site — and the
  bind itself renders the same card. The `Hover` carries the token's range. Op / node-kind /
  prelude-type content is unchanged.
- **Tests (6):** prose and comments do not hover, a var use hovers its binding, a param hovers as a
  parameter, an op still renders its signature, a node kind still hovers, and the range covers the
  token.


## Notes
- `word_at` and `scan_symbols` both become dead once L-85 and L-86 land — delete them rather than
  leaving a second, less-correct resolution path in the file.
