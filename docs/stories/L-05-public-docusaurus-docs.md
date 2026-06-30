---
id: L-05
title: Publish public Docusaurus docs
pillar: Language
status: done
note: public Docusaurus site scaffold under `website/`, with Flux-Lang text syntax/semantics docs and GitHub Pages deployment to `codewandler.github.io/flux`
---

# Publish public Docusaurus docs

## Goal
Create a public documentation surface for flux at `codewandler.github.io/flux`, clearly separated from
the repository's internal contributor/design/story docs.

## Acceptance
- [x] A `website/` Docusaurus site builds as a standalone Node package.
- [x] The public docs distinguish user-facing documentation from internal repo docs.
- [x] The public Flux-Lang section covers the text syntax, execution semantics, AST reference, and examples.
- [x] GitHub Actions builds the site and deploys `main` to GitHub Pages.

## Progress
- Added the initial Docusaurus site, public docs IA, GitHub Pages workflow, and build validation.

## Notes
- Internal docs remain in `docs/` and crate-level `docs/` directories.
- Future work: generate the public AST node catalog from `flux_lang::schema::node_kind_catalog()`.
