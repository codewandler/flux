---
id: L-106
title: "Deprecation diagnostics for every legacy spelling"
pillar: Language
status: backlog
priority: 24
epic: flux-syntax-simplification
design: docs/designs/flux-syntax-simplification.md
areas: [flux-lang, flux-lsp]
note: "P5a — the strict parser warns (with the canonical replacement) on the nine legacy dimensions; the LSP offers a canonicalize quick-fix"
---

# Deprecation diagnostics for every legacy spelling

## Goal

After L-103/L-104 ship, the strict parse path emits a deprecation diagnostic — naming the
canonical replacement — for each legacy spelling: `$`-sigiled ordinary locals, `op({k: v})`
single-object calls, `do op` calls, space-keyword control headers, body-line `until`,
`await … when` suffix form, bare-ms numbers in duration positions, and the `race timeout:` alias.
The tolerant CST keeps accepting everything (editor buffers must stay useful); the LSP surfaces the
diagnostic with a "canonicalize" code action backed by the L-103 formatter.

## Acceptance

- [ ] Failing-first: a fixture per legacy dimension asserts one diagnostic with the canonical
      replacement text and an exact source range; canonical spellings produce zero diagnostics.
- [ ] Diagnostics are warnings — `parse`/`parse_program` still succeed (removal is L-107's).
- [ ] The LSP publishes them and offers the quick-fix (verified in flux-lsp's diagnostic tests).
- [ ] `CHANGELOG` + `WHATS-NEW` announce the deprecation window and the removal release.

## Progress
-

## Notes

- The appendix table from L-105 is the enumerated contract; keep the diagnostic list generated
  from or tested against it so the two can't drift.
- Fixture discipline per crate AGENTS.md: fixtures that must stay malformed must be malformed
  lexically — these fixtures are the opposite (valid-but-deprecated), so they belong in their own
  test file, not mixed into malformed-input suites.
