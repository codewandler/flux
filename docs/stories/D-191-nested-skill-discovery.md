---
id: D-191
title: Discover nested/namespaced skill trees one-to-N levels deep
pillar: Agent
status: done
priority:
epic: claude-interop
design: docs/designs/claude-interop.md
note: ".claude/skills/<ns>/<name>/SKILL.md trees are invisible today — discovery is one level deep"
---

# Discover nested/namespaced skill trees one-to-N levels deep

## Goal
Skill discovery currently finds flat `*.md` files and one-level `<name>/SKILL.md` dirs
(`crates/flux-system/src/lib.rs`, `read_dir_text_files_with_nested`); Claude Code setups with
namespaced trees (`.claude/skills/<ns>/<name>/SKILL.md`) silently lose those skills. Recurse the
tree so any depth of `…/SKILL.md` is found, with dedup, naming, and jail semantics preserved.

## Acceptance
- [x] Discovery recurses skill roots to find `SKILL.md` at any depth (bounded — pick and document a
      sane max depth); flat `.md` files still only at the top level; failing-first test with a
      two-level tree.
- [x] Naming: frontmatter `name` wins; fallback remains the immediate parent directory name; a
      namespaced duplicate (`a/foo` vs `b/foo`) resolves by the existing first-wins dedup and warns.
- [x] Symlink jail unchanged for project roots: a nested symlink escaping the workspace is rejected
      (extend the existing confinement tests in `crates/flux-runtime/src/metadata.rs` and
      `crates/flux-system`).
- [x] Skill dirs skipped as non-skill infrastructure (e.g. a nested `references/` containing its own
      `.md`) are NOT picked up as skills; test.
- [x] Docs: claude-compat page's "what loads from where" section notes nesting support.

## Progress
- `crates/flux-system/src/lib.rs`: `System::read_dir_text_files_with_nested` now recurses via a new
  private `collect_nested_file` depth-first helper, bounded by a new `System::NESTED_FILE_MAX_DEPTH
  = 4` constant (documented: depth 1 = historical `<name>/SKILL.md`, depth 2 = Claude's namespaced
  `<ns>/<name>/SKILL.md`, plus headroom for a sub-namespace). Flat `*.md` collection is unchanged —
  still top-level only. A directory that directly contains `SKILL.md` claims its whole subtree and
  the search returns without descending further, so a skill's own `references/` (or any other
  internal directory) is never visited, let alone surfaced as a skill. Every path continues through
  `Workspace::resolve_read`, so the existing symlink jail applies at every depth, not just the top
  level.
- `crates/flux-runtime/src/metadata.rs`: `extend_skills` now emits `tracing::warn!` (crate gained a
  `tracing` dependency, matching `flux-provider`'s existing pattern) when a name collision shadows
  an already-discovered skill, naming the losing skill and its source path — the dedup logic itself
  (first-wins via `seen: HashSet<String>`) was already correct and unchanged. The parent-directory
  naming fallback (`parse_skill`, unchanged) already uses the immediate parent, so it does the right
  thing for namespaced paths (`ns/foo/SKILL.md` falls back to `foo`) with no code change needed.
- New tests (failing before this change):
  - `flux-system::tests::discovers_namespaced_skill_md_two_levels_deep` — two-level namespaced tree.
  - `flux-system::tests::nested_skill_md_beyond_max_depth_is_not_found` — depth bound enforced.
  - `flux-system::tests::a_skill_directory_claims_its_subtree_so_references_is_not_a_separate_skill`
  - `flux-system::tests::rejects_nested_symlink_escape_below_the_top_level`
  - `flux-runtime::metadata::tests::nested_namespaced_skill_symlink_escape_is_rejected` (extends the
    existing top-level confinement test one level deeper)
  - `flux-runtime::metadata::tests::a_skill_directory_with_references_is_not_double_discovered`
  - `flux-runtime::metadata::tests::namespaced_duplicate_skill_names_dedup_first_wins_and_warn` —
    captures the `tracing::warn!` via a minimal hand-rolled `tracing::Subscriber` (no new
    `tracing-subscriber` dependency) and asserts both the dedup result and the warning text.
  - Both new `flux-runtime` tests that call `discover_skills` repoint `HOME` (under the existing
    `HOME_LOCK` mutex pattern already used by a sibling command-discovery test in the same file) to
    an empty temp dir, since the real developer machine's `~/.claude/skills` otherwise leaks into
    the discovered set and breaks exact-list assertions.
- Docs: `website/docs/agent/claude-compat.md` "Skills: what loads from where" section rewritten to
  describe the shipped nesting (depth bound, `references/`-claimed-by-parent semantics, dedup+warn)
  instead of stating nested trees are not discovered. Left `skills-and-roles.md` unchanged — its
  existing wording ("a directory containing `SKILL.md`") was already accurate and links to
  `claude-compat.md` for the depth/namespacing detail.
- Scope note: `crates/flux-skill/src/lib.rs`'s own `discover`/`discover_merged`/`discover_dir`
  (the legacy, filesystem-owning implementation, documented in-file as superseded by
  `flux_runtime::metadata`) was **not** touched — it stays one-level-deep. Its tests
  (`discovers_md_files_and_skill_dirs`, `discover_merged_precedence_project_wins`, etc.) still pass
  unchanged. Reconciling/deleting that dead path is D-192's job, called out explicitly in this
  story's Notes.
- Verification (targeted, not the full workspace gate per instruction):
  - `cargo test -p codewandler-flux-system -p codewandler-flux-runtime -p codewandler-flux-skill` —
    104 + 16 + 122 passed, 0 failed (plus 0 doc-tests each).
  - `cargo clippy -p codewandler-flux-system --all-targets -- -D warnings` — clean.
  - `cargo clippy -p codewandler-flux-runtime -p codewandler-flux-skill --all-targets -- -D warnings`
    — clean.
  - `cargo fmt -p codewandler-flux-system -p codewandler-flux-runtime -p codewandler-flux-skill` then
    `-- --check` — clean (scoped to these three crates rather than `--all`, since a concurrent agent
    was mid-edit on unrelated files elsewhere in the same shared worktree; `git status`/diff
    confirmed after the fact that no unrelated file's content changed, only formatting-clean files
    already matched).
  - `cargo test -p flux-cli --test website_contract` — 13 passed, 0 failed.

## Notes
- Keep it in `flux-system`'s reader so both the runtime path and any remaining `flux-skill`
  consumers (post-D-192) share one traversal.
