---
id: C-161
title: User-defined review checks — project criteria layered over the built-in reviewer roles
pillar: Core
status: backlog
priority:
epic:
design:
note: "flux review runs three embedded reviewer roles (review.rs:100-108) overridable only wholesale via .flux/agents/review-*.md — there is no per-CRITERION project check with its own severity and path scope, though --fail-on <severity> already exists (args.rs:381-384) so the severity plumbing is done"
---

# User-defined review checks — project criteria layered over the built-in reviewer roles

## Goal
Let a repository state *what its own review cares about* as data — `.flux/checks/*.md` files, one
criterion each — instead of forcing an all-or-nothing override of flux's embedded reviewer roles.
A check declares its severity, the paths it applies to, and the ops it may use; `flux review` runs
the built-in protocol plus every matching project check, and `--fail-on` gates on the union.

## Acceptance
- [ ] `.flux/checks/*.md` files with frontmatter (`name`, `description`, `severity`, `globs`,
      optional allowed ops) are discovered and run as additional criteria alongside the embedded
      `strict_review` roles — failing-first test with a fixture check that fires on a seeded defect.
- [ ] A check's `globs` scope it: a check declaring `globs: ["crates/flux-lang/**"]` contributes
      nothing to a review whose `--files` are all outside it, pinned by test.
- [ ] A check's declared severity is what its findings carry into the `ReviewReport`, so the
      existing `--fail-on <severity>` threshold (`args.rs:381-384`) gates on it unchanged.
- [ ] A check's allowed-op list narrows the op surface for that check only, and the read-only
      invariant of `flux review` still holds (`review.rs:107-108` — reviewer roles declare
      `tools: []`; a check may not widen past read-only). Test asserts a check declaring a writing
      op is rejected at load with a named diagnostic, not silently dropped.
- [ ] Layering is documented and tested: user-global checks (`~/.flux/checks/`) are overridden by
      project checks of the same name (the skill-discovery precedence rule, not a new one).
- [ ] Website docs page for authoring a check, with one worked example.

## Progress
- (not started — filed from the 2026-07-28 Amp feature-mining pass)

## Notes
- Source: [../research/amp.md](../research/amp.md) — Amp's `.agents/checks/` with YAML frontmatter,
  configurable severity and per-check tool access, and project-over-global precedence.
- Evidence the gap is real: `crates/flux-cli/src/review.rs:100-108` — the roles and the
  `strict_review` flow text ship *in the binary*, with `.flux/agents/review-*.md` as a whole-role
  override; there is no per-criterion extension point.
- Reuse rather than invent: frontmatter parsing, glob scoping, and the
  name-collision precedence rule all exist in `flux-skill`'s discovery layer (D-186…D-192).
- Deliberately **not** a new review engine — checks are inputs to the existing L-13 strict-review
  protocol.
