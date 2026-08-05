---
id: C-425
title: "A compact recipe demonstrates the native planning-board surface"
pillar: Core
status: backlog
design: docs/designs/flux-recipes.md
epic: flux-recipes
areas: [examples, docs]
note: "Decision 0010 makes tracking a supported product; this later recipe must consume `flux board`, not reimplement Track invariants in a Program"
---

# A compact recipe demonstrates the native planning-board surface

## Goal

After the native board product lands, ship one readable example that queries a planning board and
reacts to its typed results without implementing another tracker.

## Acceptance

- [ ] The example consumes a declared planning board and its typed operations; it does not parse
      frontmatter, regenerate Markdown, allocate ids or maintain a parallel status model.
- [ ] It demonstrates one semantic/model step over runtime-validated board data and names the
      mechanical invariants owned by `flux board`.
- [ ] It links to `flux board skill` and remains readable in one sitting, with clean-machine
      prerequisites in its first five lines.
- [ ] The whole examples sweep and full repository gate pass.

## Progress

- 2026-08-05 — respecified by Decision 0010 and returned to backlog. The former unsupported tracker
  recipe would compete with the now-chartered product.

## Notes

- Depends on C-549. C-426 may still provide a determinism example after this shape exists.
