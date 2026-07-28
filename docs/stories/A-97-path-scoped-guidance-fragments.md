---
id: A-97
title: Path-scoped guidance fragments — load project conventions only when they apply
pillar: Agent
status: backlog
priority:
epic:
design:
note: "ProjectFiles reads CLAUDE.md/AGENTS.md/.flux/context.md WHOLE and unconditionally every turn (context.rs:29-73) — guidance is all-or-nothing, so a monorepo pays for every subsystem's conventions on every turn; skills carry `triggers` but no path scope (flux-skill lib.rs:53-78)"
---

# Path-scoped guidance fragments — load project conventions only when they apply

## Goal
Let a repository split its conventions into fragments that load **only when the turn touches
matching paths**. Today `ProjectFiles` reads `CLAUDE.md` / `AGENTS.md` / `.flux/context.md` in full
on every turn (`context.rs:29-73`), so a large repo either keeps its guidance thin (and the agent
misses subsystem rules) or pays the whole thing on every turn (and buries the relevant rule in
noise). Path scoping makes guidance grow with the repo without growing the prompt.

## Acceptance
- [ ] A guidance fragment declaring `globs:` in frontmatter contributes its body to the turn's
      context **only** when the turn's resolved path set matches — failing-first test with two
      fragments where exactly one fires.
- [ ] A fragment with no `globs` behaves exactly as today (always loaded), so existing
      `AGENTS.md` / `CLAUDE.md` files are unaffected — pinned by the existing `context.rs` tests
      continuing to pass unchanged.
- [ ] Fragments are referenced from the root guidance file rather than auto-discovered by walking
      the tree, so what loads is auditable from one place, and a referenced-but-missing fragment is
      a named warning rather than a silent omission.
- [ ] The confinement invariant at `context.rs:49-53` holds for fragments too: they are read
      through a deliberately confined workspace, never the agent's possibly-widened tool workspace.
      Test asserts a fragment path escaping the root is refused.
- [ ] The "resolved path set" for a turn is defined and documented (at minimum: paths named in the
      prompt and paths the turn has already touched) — an honest, testable rule, not a heuristic
      that silently drifts.

## Progress
- (not started — filed from the 2026-07-28 Amp feature-mining pass)

## Notes
- Source: [../research/amp.md](../research/amp.md) — Amp's AGENTS.md `@`-mentions plus YAML
  frontmatter `globs` on the mentioned files ("granular guidance").
- Evidence the gap is real: `crates/flux-runtime/src/context.rs:29-73` (`ProjectFiles` — a fixed
  three-file list, read whole, every turn); `crates/flux-skill/src/lib.rs:53-78` (`Skill` carries
  `triggers`, `allowed_ops`, `model` — no path scope).
- Directly serves the cache work (**C-133…C-140**): guidance sits in the stable prefix, so a
  fragment that loads or unloads mid-session is a cache invalidation. Scoping must be resolved
  **once per turn**, before the prefix is built — not re-evaluated per round. State this as an
  invariant in the design, or this feature will quietly halve the cache the way A-95 describes.
- Naming: flux should not adopt Claude's `@`-mention syntax without deciding it is the right
  spelling for flux; the mechanism matters more than the sigil.
