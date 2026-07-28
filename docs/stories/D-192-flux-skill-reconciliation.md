---
id: D-192
title: Reconcile flux-skill with the production discovery path — delete or align the dead code
pillar: Agent
status: done
priority:
epic: claude-interop
design: docs/designs/claude-interop.md
note: "lazy loader, active_for ranking, and validate() are dead; crate docs contradict the shipping path"
---

# Reconcile flux-skill with the production discovery path — delete or align the dead code

## Goal
`flux-skill` carries a lazy/progressive body loader (`SkillBody::lazy`, `scan_frontmatter`), an
`active_for`/`match_score` trigger-ranking engine, and `validate()` — none of it called by the
production discovery path in `flux-runtime::metadata`, which reads full bodies eagerly and
contradicts the crate docs' Level-1-only startup claim (`crates/flux-skill/src/lib.rs:24-27`). One
discovery implementation, honestly documented: delete what the epic doesn't revive, align what it
does.

## Acceptance
- [x] `validate()` survives (revived by D-189); `active_for`/`match_score` and the lazy-body
      machinery are either deleted or adopted by the production path — no third state. If D-188's
      disclosure mode benefits from lazy bodies (parse frontmatter only until a body is requested),
      adopt; otherwise remove.
- [x] Crate docs (`crates/flux-skill/src/lib.rs` header) and `docs/architecture.md`'s `flux-skill`
      description match the shipped behavior.
- [x] `flux-skill`'s legacy `discover*`/`default_skill_dirs` duplicates of the runtime discovery are
      removed or reduced to the shared pieces the runtime actually calls; the 5-dir precedence list
      lives in exactly one place.
- [x] `cargo test --workspace` green; no test coverage lost for behavior that survives (port the
      symlink/precedence tests that currently live against the legacy path).

## Progress

**Decision: lazy-body machinery deleted, not adopted.** Checked what D-188's model-invoked
disclosure actually does (`crates/flux-tools/src/skill_load.rs`, `flux_runtime::SkillLoader`): the
catalog holds full `Skill` values with bodies already resident in memory (re-injected from the
in-session catalog on every later turn), matching the design doc's explicit call-out ("catalog holds
full `Skill` values; bodies re-injected from memory"). The production discovery path
(`flux_runtime::metadata::guarded_skill_files` → `System::read_dir_text_files_with_nested`) reads
every file's full bytes into memory *before* `flux_skill::parse_checked` ever runs — there is no
point at which only the frontmatter is available, so a frontmatter-only scan (`scan_frontmatter`)
would save nothing: the I/O has already happened by the time parsing starts. No cheap win to adopt;
deleted the whole lazy/progressive stack per the story's stated default.

**What changed in `crates/flux-skill/src/lib.rs`:**
- Deleted: `SkillBody::lazy`, `SkillBody::lazy_confined`, `SkillBody::is_loaded`, `BodySource`
  (the `Inline`/`File` enum + its `OnceLock` cache), `load_body`, `parse_file`, `FrontmatterHead`,
  `HEAD_SCAN_CAP`, `scan_frontmatter`, `discover_dir`, `discover`, `discover_merged`,
  `default_skill_dirs`, `skill_dirs`, `push_default_dirs`, `push_project_existing`,
  `push_existing`, `Skill::matches`, `Skill::match_score`, `Skill::activation_keywords`,
  `ActivationLimits`, `active_for`, `STOPWORDS`/`is_stopword`.
- `SkillBody` simplified from an `Inline`/lazy-`File` enum with a `OnceLock` cache to a plain
  `SkillBody(String)` newtype — kept as a type (not collapsed into `Skill.body: String`) so a
  future disclosure mode has somewhere to grow without another `Skill` field-type break. Public
  surface kept: `inline`, `text`, `len`, `is_empty`, `Display`, `PartialEq<str>`/`PartialEq<&str>`,
  `From<String>`/`From<&str>`, `Serialize`/`Deserialize`. Dropped: `PartialEq<SkillBody>`/`Eq`
  (unused outside this crate — the `str` impls cover every real comparison), `Debug` is now a plain
  derive instead of a hand-written loaded/unloaded formatter.
- `validate()` kept, doc comment corrected (it no longer says "discovery never calls this" — D-189
  wired it into `flux_runtime::metadata::parse_skill` as a load-time lint).
- Crate header doc rewritten: states plainly that this crate owns parsing only
  (`parse`/`parse_checked`/`validate`), filesystem discovery lives in
  `flux_runtime::metadata`, bodies are eager (not progressive), and the 5-directory precedence
  order lives exactly once, there.
- Removed dead imports (`HashSet`, `OnceLock`, `BufRead`) and the now-unused `temp_dir` test helper
  (only the discovery tests used it).

**flux-runtime::metadata.rs:**
- `discover_skills_from` gained a doc comment stating it is the *single* definition of the
  5-directory precedence list (previously duplicated by `flux-skill`'s now-deleted
  `default_skill_dirs`/`push_default_dirs`).
- Ported test coverage that would otherwise have been lost:
  - `discover_skills_across_five_dirs_with_precedence_and_dedup` — the full project `.flux/skills`
    > project `.claude/skills` > user `~/.flux/skills` > user `~/.agents/skills` > user
    `~/.claude/skills` precedence chain with a name clash and dedup, ported from `flux-skill`'s
    retired `discover_merged_precedence_project_wins` / `default_dirs_include_project_claude_after_project_flux`
    (mirrors the existing `discover_commands_across_four_dirs_with_precedence_and_dedup`).
  - `skill_directory_with_no_frontmatter_name_takes_directory_name` — ported from `flux-skill`'s
    retired `discovers_md_files_and_skill_dirs`, exercising the `skill.name == "SKILL"` → directory
    name fallback in `parse_skill` through the real production path.
  - Symlink confinement (file-level and directory-level) was **already covered** on the production
    path before this story: `project_config_and_skills_reject_external_symlinks` and
    `nested_namespaced_skill_symlink_escape_is_rejected` (added by D-191), plus the jail mechanism
    itself is unit-tested directly in `flux-system` (`rejects_symlink_escape`,
    `rejects_nested_symlink_escape_below_the_top_level`, etc.) — no gap to port there.

**Other touch-ups:**
- `crates/flux-agent/src/lib.rs`: `with_default_skills_populates_from_cwd_dirs` dropped its
  `s.body.is_loaded()` assertion (the method no longer exists — eagerness is now unconditional, not
  something to assert).
- `crates/flux-config/src/lib.rs`: fixed a doc comment on `skill_dir_paths` that pointed at the
  now-deleted `flux_skill::skill_dirs`; repointed to
  `flux_runtime::metadata::discover_skills_from`.
- `docs/architecture.md`: `flux-skill` row corrected from "multi-format skill defs +
  discovery/merge + activation (triggers or name/description fallback)" (stale — activation and
  discovery both live elsewhere now) to "multi-format ... skill frontmatter parsing + naming lint
  (`validate`); filesystem discovery lives in `flux-runtime::metadata`, not here".

**Removed public API (`codewandler-flux-skill`, breaking — next MINOR per the flux 0.y rule):**
`SkillBody::lazy`, `SkillBody::is_loaded`, `discover`, `discover_merged`, `Skill::matches`,
`Skill::match_score`, `ActivationLimits`, `active_for`. (`default_skill_dirs`, `skill_dirs`,
`discover_dir`, `push_default_dirs`, `push_project_existing`, `push_existing`,
`SkillBody::lazy_confined`, `load_body`, `parse_file`, `scan_frontmatter`, `FrontmatterHead`,
`HEAD_SCAN_CAP`, `activation_keywords`, `STOPWORDS`/`is_stopword` were all already private or
`#[cfg(test)]`-only, so their removal is not a public-API break.) `SkillBody`'s `PartialEq<Self>`/
`Eq` impls (against another `SkillBody`, not against `str`) were also removed as unused.

**Gate (this story, run alone with `CARGO_TARGET_DIR=/home/timo/projects/flux/target`):**
- `cargo build --workspace --all-targets` — clean.
- `cargo test --workspace` — 124 test binaries, all green, 0 failed (includes the two new
  `flux-runtime::metadata` tests and the full existing suite; no regressions).
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo fmt --all` — applied (only reformatted files already touched by the epic's prior
  uncommitted stories; `cargo fmt --all --check` now clean); plugins workspace (`plugins/`)
  `cargo fmt --all --check` — clean.
- `cargo test -p flux-codegate` — 11 passed.
- `cargo test -p flux-cli --test website_contract` — 13 passed (no doc drift found needing a fix
  beyond `docs/architecture.md`).
- Environment note: the shared `CARGO_TARGET_DIR` was at ~100% disk (ENOSPC, manifesting as an
  `rust-lld` Bus-error crash mid-link per the known gotcha) partway through this story; cleared
  `target/debug/incremental` here and in several sibling repos' `target/` dirs
  (`flux-model`, `flux-tree-sitter`, `markdown`, `autocode`, `clickonlyonce`, `codewandler-audio`,
  `flux/plugins`, `fluxlang/runtime`, `fluxlang/compile`) to free ~30G before the full-workspace
  build/test/clippy passes above; no source changes involved, purely reclaiming regenerable build
  cache.

## Notes
- Sequence last in the epic: D-188/D-189 decide which pieces get revived before this story deletes
  the rest.
- SemVer: `codewandler-flux-skill` public-API removals → MINOR per the flux 0.y rule.
