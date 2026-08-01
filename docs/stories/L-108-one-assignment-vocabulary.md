---
id: L-108
title: "One assignment vocabulary — `key: value` in module decls and ctx"
pillar: Language
status: backlog
priority: 34
epic: flux-syntax-simplification
design: docs/designs/flux-syntax-simplification.md
areas: [flux-lang]
note: "P6 — retire the third `key value` spelling; rename ctx's `budget` to `chars:` to end the budget-block collision"
---

# One assignment vocabulary — `key: value` in module decls and ctx

## Goal

Three assignment vocabularies coexist: `key: value` (calls/options/templates), bare `key value`
(module-declaration attribute lines, `ctx`'s `purpose`/`budget` lines), and `x = v` (binds).
Adopt `key: value` in module declarations (`kind: slack`, `bot_token: secret "X"`) and in `ctx`
(`ctx pack, purpose: "…", chars: 8000` with `include`/`exclude` staying structural lines), and
rename `ctx`'s `budget` to `chars:` — today `budget` means "dispatch cap" as a block and "char
budget" inside `ctx`, one keyword with two units.

## Acceptance

- [ ] Failing-first: canonical decl/ctx fixtures parse to the identical AST as their current
      spellings; `format` emits the new form.
- [ ] Old spellings remain accepted during the L-106 window and join its deprecation table.
- [ ] `chars:` (or the chosen name) replaces `budget` in `ctx` headers with the same AST field;
      the `budget` block keyword is untouched.
- [ ] Docs (`syntax.md` module-declarations + ctx sections), skill examples, and the website
      mirror updated in the same commit; goldens regenerated via `FLUX_UPDATE_GOLDEN=1` then
      verified unset.

## Progress
-

## Notes

- Naming open question for the design discussion: `chars:` is literal (the cap is characters,
  `analyze`/`runtime` shrink by char budget); `tokens:` reads better but would lie about the unit.
