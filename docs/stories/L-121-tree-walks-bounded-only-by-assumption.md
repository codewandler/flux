---
id: L-121
title: "The remaining CST walks are bounded only by assumption, and rowan's own `Drop` aborts at ~4,000 levels"
pillar: Language
status: ready
priority: 19
epic: flux-lang-hardening
design: docs/designs/flux-lang-hardening.md
areas: [flux-lang, flux-lsp]
note: "found by L-114, which guarded the block walk and then measured what it left standing. The pattern L-114 proved matters: `Parse` has public fields, so 'the parser already bounds this' is an assumption about callers, not a property of the type"
---

# The tree walks that are still bounded only by assumption

## Goal

Close the recursion hazards [L-114](L-114-statement-depth-guard.md) measured but deliberately left
outside its scope, and decide what to do about a limit that is **not flux's code at all**.

L-114 guarded `block_if_indented` (parser) and the block walk (`cst_decode`), and in doing so
established the argument that makes the rest of these worth fixing: `crate::parser::Parse` has
**public fields**, so `cst_to_draft` / `cst_to_module` can be handed a tree the parser never built.
"The parser already caps this" is therefore an assumption about every caller, not a property of the
type — which is exactly why L-114 gave the lowerer its own `MAX_LOWER_DEPTH` rather than leaning on
`MAX_PARSE_DEPTH`.

Three things are left standing:

1. **`cst_decode::lower_expression` recurses per nested expression with no guard of its own.**
   Bounded today only by the parser's expression cap — the same footing the block walk was on before
   L-114.
2. **`crate::parser::Parse`'s public fields** are the reason (1) is reachable at all. Whether the fix
   is a guard, a constructor invariant, or sealing the type is this story's decision.
3. ⚠ **`rowan`'s green-node `Drop` is itself recursive and aborts the process at ~4,000 levels of
   nesting — this is upstream code, not flux's.** Measured by L-114: 2,000 levels drop in 1.3 ms,
   4,000 levels `SIGABRT`. It is *not* reachable from `.flux` source now that the parser caps tree
   depth, but any code that builds a green tree by hand can abort the process **just by dropping
   it** — an LSP synthesiser, a codegen path, or a fuzz harness. This is why it matters for
   [L-119](L-119-raw-text-fuzzing.md) in particular: a fuzzer that constructs trees can take the
   process down in cleanup and look like a crash in the code under test.

## Acceptance

- [ ] **Failing-first**: a test that drives `lower_expression` past its ceiling through a
      hand-built tree (not through `parse`, which would cap it first), failing at the merge base
      with an abort or overflow.
- [ ] `lower_expression` is bounded on its own terms, sharing the `MAX_LOWER_DEPTH` budget L-114
      established rather than introducing a third ceiling.
- [ ] The `Parse` public-field hole is **decided and recorded** — guarded, sealed, or explicitly
      accepted with the reason written at the type. Do not leave it as an unstated assumption; that
      is the thing this story exists to remove.
- [ ] The rowan `Drop` limit is **documented where a tree gets built by hand**, with the measured
      numbers, so the next author of an LSP synthesiser or fuzz harness meets it in the code rather
      than in a mysterious `SIGABRT`. Decide explicitly whether flux can defend against it at all
      (it is upstream's recursion, in a `Drop` impl) or whether documenting the ceiling is the
      honest answer.
- [ ] [L-119](L-119-raw-text-fuzzing.md)'s Notes point at this story, so whoever writes the fuzz
      harness does not lose a day to it.
- [ ] Full gate green in both workspaces.

## Notes

- L-114's shared-counter design is the precedent: one budget, because it is one stack. A third
  independent ceiling would reintroduce exactly the drift its
  `const _: () = assert!(MAX_LOWER_DEPTH > MAX_PARSE_DEPTH)` exists to prevent.
- ⚠ `crates/flux-lang/src/cst_decode.rs` (2,674 lines) had **no test module at all** before L-114,
  which added one holding only its own two tests. The rest of the module is still covered only
  indirectly through `parse`/`parse_program`. That is not this story's job to fix wholesale, but it
  is why a bug here would not be caught by the existing suite.
- The depth budget L-114 shares between statements and expressions means a statement at block depth
  *d* leaves `MAX_PARSE_DEPTH − d` levels for its expressions. Any ceiling this story adds inherits
  that coupling.

## Progress

- Filed 2026-08-01 from L-114's measured findings, which corrected the review's headline in passing:
  at the base `parse_cst` alone *survives* 2,000 levels (253 ms, zero diagnostics); `parse()` and
  `format_source()` are what abort, and the parser itself only goes over at ~6,000. The lowerer, not
  the parser, was the first thing to fall.
