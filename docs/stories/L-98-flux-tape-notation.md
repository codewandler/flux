---
id: L-98
title: "Flux Tape — a flat path-addressed transport notation"
pillar: Language
status: ready
priority: 23
epic: flux-notation-workbench
design: docs/designs/flux-notation-workbench.md
areas: [flux-lang]
note: "Every line locates its AST node; indentation is cosmetic and malformed path structure fails closed"
---

# Flux Tape — a flat path-addressed transport notation

## Goal

Encode structured Flux as self-locating lines suitable for streaming, patches, transit, and precise
diagnostics without relying on an indentation stack.

## Acceptance

- [ ] A documented address grammar covers body indexes, named branches, cases/defaults, and nested
      bodies without conflating labels with numeric positions.
- [ ] Failing-first tests encode and decode the shared triage AST independent of line indentation.
- [ ] The reader rejects duplicate addresses, missing parents, incompatible node/arm kinds,
      ambiguous ordering, and non-contiguous required body positions.
- [ ] Native-core plus raw-node escape property tests satisfy `parse_tape(format_tape(ast)) == ast`.
- [ ] Formatting is deterministic and diagnostics name the exact Tape address and input line.
- [ ] Tape is selected explicitly and changes no canonical `.flux` or runtime behavior.

## Progress

- (not started)
