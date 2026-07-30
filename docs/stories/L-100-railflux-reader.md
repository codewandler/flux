---
id: L-100
title: Parse the stabilized Railflux subset back into DraftAst
pillar: Language
status: blocked
epic: flux-notation-workbench
design: docs/designs/flux-notation-workbench.md
areas: [flux-lang]
note: "DEFERRED after L-95 — accept only formally unambiguous rails; visual resemblance never authorizes a guessed AST"
---

# Parse the stabilized Railflux subset back into `DraftAst`

## Goal

After Railflux output has stabilized in real terminal use, make its formal core executable by
parsing unambiguous diagrams into the same AST and canonical Flux source.

## Acceptance

- [ ] L-95 is done and its canonical output grammar has a versioned written specification before
      this story begins.
- [ ] Failing-first tests parse the shared triage diagram into the identical canonical Flux AST.
- [ ] The accepted subset defines connector geometry, joins, branch labels, ordering, calls,
      bindings, confirmation gates, and returns without whitespace-dependent guesswork.
- [ ] Renderer-produced diagrams in the accepted subset satisfy
      `parse_railflux(render_railflux(ast)) == ast`.
- [ ] Diagrams outside that subset fail with line/column diagnostics identifying the ambiguous or
      unsupported rail; the parser never selects one of several plausible ASTs.
- [ ] Input requires an explicit Railflux source kind and still traverses ordinary Flux analysis,
      authorization, approval, and guarded execution after conversion.

## Progress

- **Blocked on L-95 — this parses the *stabilized* Railflux subset, and the story says "after L-95" in its own Notes. Until the renderer's output is stable there is no subset to parse back.** Recorded rather than left in `backlog`, so the board says *why* it is not takeable instead of implying nobody has decided.

- (deferred — depends on L-95 output stabilizing)
