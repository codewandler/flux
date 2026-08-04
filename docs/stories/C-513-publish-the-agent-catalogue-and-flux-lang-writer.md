---
id: C-513
title: "Publish the complete agent catalogue and add the Flux-Lang writer"
pillar: Core
status: ready
priority: 0
epic: flux-lang-writer
areas: [flux-agent, flux-lang, docs]
note: "owner-directed first slice — add the flux-lang-writer role and make every shipped or tracked agent role discoverable through a GitHub-source-linked, census-tested public catalogue"
---

# Publish the complete agent catalogue and add the Flux-Lang writer

## Goal

Let a user discover which agent roles Flux ships and which roles this repository adds, inspect the
exact source of each role on GitHub, and delegate Flux-Lang authoring to a purpose-built
`flux-lang-writer` without mistaking an instruction profile for a new authority boundary.

## Acceptance

- [ ] `.flux/agents/flux-lang-writer.md` is a tracked project role with `profile: coding` and an
      explicit tool allow-list sufficient to inspect, create, edit, and validate `.flux` files. Its
      instructions require reading `crates/flux-lang/AGENTS.md` plus the relevant language reference,
      making the smallest workspace-relative Flux-Lang change, and reporting the exact validation
      evidence.
- [ ] The writer distinguishes syntax/analysis validation from execution. It never runs an effectful
      flow merely to check it; if execution is explicitly requested, it uses the ordinary Flux
      runtime so authorization, approval, guarded IO, sandboxing, and redaction remain in force.
- [ ] `website/docs/agent/skills-and-roles.md` contains a complete, scannable catalogue of the six
      embedded roles (`scout`, `planner`, `worker`, `reviewer`, `evaluator`, `summarizer`) and every
      tracked `.flux/agents/*.md` project role, including `flux-lang-writer`.
- [ ] Every catalogue entry links directly to its canonical
      `https://github.com/codewandler/flux/blob/main/...` source and states whether it is an embedded
      fallback or a repository-defined override/example. Ignored local `.flux/agents` scaffolding is
      described as local-only and is not included in the GitHub-backed inventory.
- [ ] A failing-first website contract derives the embedded role set from
      `crates/flux-agent/assets/roles/*.md` and the project role set from tracked repository files,
      then proves that the public page has exactly one source link for every role. Adding or removing
      a role without updating the catalogue fails deterministically.
- [ ] The role file has an explicit `.gitignore` exception, the customer and engineering changelogs
      announce the new discoverable role/catalogue without claiming a new security boundary, the
      generated story board and embedded public docs are current, and the full repository gate is
      green.

## Progress

- (not started)

## Notes

- The GitHub inventory is based on tracked files (`git ls-files`), not every ignored file currently
  present in one developer checkout.
- The page already documents role lookup and capability narrowing; extend that canonical page rather
  than creating a second role system or duplicated tutorial.

