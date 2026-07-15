---
id: C-85
title: Guard model-driven tool mutations (git_checkout pathspec, edit empty old_string)
pillar: Core
status: done
priority: 12
epic: harness-hardening
design: docs/designs/harness-hardening.md
note: "Correctness (Medium) — git_checkout '.' discards uncommitted work; edit empty old_string corrupts a file"
---

# Guard model-driven tool mutations (git_checkout pathspec, edit empty old_string)

## Goal
Two destructive tool footguns a model can trigger with legitimate-looking args: `git_checkout` forwards
a model-controlled ref verbatim with no `--`, so `branch:"."` makes git treat it as a pathspec and
`git checkout .` silently discards all uncommitted changes (and `permission_subjects` is a constant, so
allowlisting the tool authorizes every value including `.`); and `edit` with an empty `old_string` +
`replace_all` runs `content.replace("", new)`, inserting `new` between every character and destroying a
file the model legitimately read.

## Acceptance
- [ ] Failing-first test: `git_checkout` with `.`/`..`/leading-`-`/path-shaped values is rejected; switch
      to `git switch`/`git switch -c` (never interprets a pathspec).
- [ ] Failing-first test: `edit` with an empty `old_string` is rejected up front (all paths, not just the
      non-`replace_all`/fuzzy ones).

## Progress
- **2026-07-15 — DONE (unit-test + clippy verified; full gate pending).** `git_checkout` now uses
  `git switch`/`git switch -c` (never interprets a pathspec) and rejects path/option-shaped refs
  (`.`, `..`, leading `-`, embedded `..`) before git runs. `edit` refuses an empty `old_string` up
  front (all paths), so `content.replace("", …)` can no longer corrupt a file. Failing-first tests
  `git_checkout_refuses_pathspec_like_ref` + `edit_rejects_empty_old_string`; 133 flux-tools tests green.

## Notes
- `crates/flux-tools/src/lib.rs:2388` (`git_checkout`), `:2366` (`permission_subjects`), `:745` (`edit`).
- Design: [harness-hardening](../designs/harness-hardening.md).
