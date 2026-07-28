---
id: L-91
title: Split the server into the designed modules, add a protocol-level harness, close the epic
pillar: Language
status: done
epic: flux-lsp-round-2
design: docs/designs/flux-lsp-round-2.md
note: the crate is Cargo.toml + README.md + one 1800-line src/main.rs, while the design specified server/document/convert/diagnostics/completion/hover/format/catalog modules (flux-lsp.md:40-43); and every test calls an internal function directly — the "drive the server over an in-memory duplex" verification (flux-lsp.md:131) was never built, so nothing proves an advertised capability has a wired handler
---

# Split the server into the designed modules, add a protocol-level harness, close the epic

## Goal

Leave `flux-lsp` in the shape its design specified, with a test that exercises it as a *server*
rather than as a bag of functions — and close the round-2 epic with docs that match what ships.

## Why (evidence)

- `crates/flux-lsp/` contains exactly `Cargo.toml`, `README.md`, and `src/main.rs` (1800 lines):
  bootstrap, capabilities, all twelve handlers, the scope model, semantic tokens, the formatter, the
  line index, and ~400 lines of tests in one file.
- The design specified the split: "`main.rs` (stdio bootstrap) · `server.rs` … · `document.rs` …
  · `convert.rs` … · `diagnostics.rs` · `completion.rs` · `hover.rs` · `format.rs` · `catalog.rs`"
  (`docs/designs/flux-lsp.md:40-43`).
- Every test invokes an internal helper (`diagnostics(text)`, `main.rs:1388`; `document_symbols`,
  `:1599`; `semantic_tokens`, `:1672`; `apply_content_change`, `:1756`). None goes through
  `LspService`, so a handler could be unwired, or `initialize` could advertise a capability nobody
  answers, with the suite still green — `range: Some(false)` alongside a full-only implementation
  (`main.rs:211`) is the shape of that risk.
- The design's Verification asked for exactly this: "drive the server over an in-memory duplex"
  (`docs/designs/flux-lsp.md:131`).

## Acceptance

- [x] `src/main.rs` is the stdio bootstrap only; the server is split into modules along the lines
      the design named, with each module's tests moving with it.
- [x] An integration test drives `LspService` over an in-memory duplex through a scripted session:
      `initialize` → `didOpen` → `didChange` → completion / hover / references / rename / format /
      documentSymbol / definition / semanticTokens → `shutdown`.
- [x] That harness asserts every capability advertised in `initialize` has a handler returning a
      well-formed response — a new advertised capability without a handler fails the suite.
- [x] `crates/flux-lsp/README.md` capability table and `website/docs/language/editors.md` match the
      shipped behaviour after L-85…L-90.
- [x] CHANGELOG entries for the epic; `WHATS-NEW.md` gets the user-visible half (better completion,
      hover, rename, formatting); the roadmap epic narrative moves planned → shipped.
- [x] Full dev-loop gate green, `flux-codegate` layering lint included.

## Progress
- **Done (2026-07-28).** `src/main.rs` is now an 11-line stdio bootstrap; the server is split into
  `server`, `document`, `completion`, `hover`, `scope`, `diagnostics`, `catalog`, `format`,
  `semantic`, `symbols` and `convert`, each carrying its own tests. The 1,795-line file the design
  complained about is gone.
- **Protocol harness** (`tests/protocol.rs`): drives `LspService` over an in-memory duplex through a
  scripted session — `initialize` → `didOpen` → `didChange` → completion / hover / references /
  rename / format / documentSymbol / definition / semanticTokens → `shutdown`.
  `every_advertised_capability_has_a_handler` walks what `initialize` advertises and fails the suite
  if any of it has no handler returning a well-formed response, which is the regression this epic
  existed to prevent.
- **Docs reconciled to shipped behaviour** — this was still outstanding when the epic was picked up
  again: both `crates/flux-lsp/README.md` and `website/docs/language/editors.md` still claimed
  "modules are left unchanged to preserve declaration order" (L-88 changed that) and listed neither
  references, rename, nor range formatting. Both tables now match `capabilities()`.
- CHANGELOG, WHATS-NEW (+ generated website mirror) and the roadmap epic narrative updated; all seven
  stories closed.
- **Tests (4 protocol):** the scripted session, capability/handler agreement, semantic tokens range +
  delta, and a non-symbol position refusing rename.


## Notes
- Deliberately **last** in the epic: split the file once, after the round-2 code that will live in
  those modules exists.
- Depends on L-85…L-90.
