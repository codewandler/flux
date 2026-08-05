---
id: L-130
title: "A first-class board declaration binds scope, profile and backend"
pillar: Language
status: done
epic: first-class-board
design: docs/designs/native-board-fleet-cli.md
areas: [flux-lang, flux-cli, flux-capabilities]
note: "Decision 0010 declaration — `board <name>` registers an explicit binding; `kind board:*` receives one bounded migration release and never enters datasource registry"
---

# A first-class board declaration binds scope, profile and backend

## Goal

Give Flux-Lang a source-linked declaration for every board axis so a Program never infers purpose or
lifetime from a backend string.

## Acceptance

- [x] Parser, analyzer, formatter, AST serialization, syntax reference and editor mirrors accept:
      `board <name>` with required `scope`, `profile` and `kind`, plus backend-specific closed fields.
- [x] Program declarations register through A-134's registry and expose only the selected profile's
      operations under the binding name. Failing-first test resolves two differently profiled boards.
- [x] Session scope binds the current session; repository scope validates a confined root; workspace
      federation validates named members. Invalid combinations are source-spanned hard errors.
- [x] Planning document roots for vision, roadmap, decisions and designs are explicit repository or
      workspace configuration and cannot escape the bound root.
- [x] `datasource kind "board:*"` never enters the datasource registry. For one release its startup
      diagnostic prints the exact replacement declaration; ambiguity or unsupported legacy fields
      refuse instead of guessing. The following removal release is documented.
- [x] Unknown kinds/profiles/scopes list supported values. Declaration order never selects a default.
- [x] Board authority subjects use A-134's `board:` grammar; no datasource-shaped board subject
      remains in tests or public docs.
- [x] Website docs, generated references and config-completeness tests are updated. Targeted
      language/CLI tests pass; the board wave owns the full gate.

## Notes

- Depends on A-134. The old design's fixed-lifecycle language is superseded by Decision 0010.
