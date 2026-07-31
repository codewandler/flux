---
id: C-353
title: Protect main and gate release secrets behind a protected environment
pillar: Core
status: backlog
epic: release-trust-residuals
design: docs/designs/release-trust-residuals.md
note: "branches/main/protection -> 404; rulesets -> []; environments -> only github-pages; merged PRs -> zero. Every assurance lane in the repo is advisory until this lands"
---

# Protect `main` and gate release secrets behind a protected environment

## Goal

Make the gates load-bearing. Until `main` refuses an unreviewed or red push, CodeQL, the corpus
lanes, `cargo-deny` and `ci.yml` are documentation.

## Acceptance

- [ ] `main` carries protection or a ruleset with required status checks (at minimum `ci`), no force
      push, and no deletion.
- [ ] A protected `release` environment exists with a deployment-branch policy restricted to tag
      refs; `RELEASE_TOKEN`, `CARGO_REGISTRY_TOKEN` and `MINISIGN_SECRET_KEY` are scoped to it
      rather than being plain repo secrets.
- [ ] The single-maintainer reality is handled explicitly — a required-review rule with one
      collaborator either gets a documented bypass or is deliberately not adopted, and the choice is
      written down in `docs/designs/release-trust-residuals.md`.
- [ ] `ci.yml`'s current red on `main` (`published host-kit is not behind the live protocol version`)
      is resolved before protection is enforced, so the first enforced state is green.
- [ ] A script or documented `gh api` sequence re-derives the protection state, so the next review
      does not have to rediscover it.

## Progress

- 2026-08-01 — filed from validation of REL-01/ASSURE-04. Highest-leverage single item in the epic.

## Notes

- This is repository configuration, not code. It still gets a story because it is the precondition
  for every other assurance claim the project makes.
