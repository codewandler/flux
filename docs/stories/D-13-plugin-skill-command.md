---
id: D-13
title: Generated plugin skill — `flux plugin skill`
pillar: Core
status: done
priority:
design: docs/designs/plugin-skill-generation.md
---

# Generated plugin skill — `flux plugin skill`

## Goal
A `flux plugin skill` command that renders installed flux-plugin manifests into a Claude-format
`flux-plugin` skill (`SKILL.md` + `references/<plugin>.md`) — the flux analogue of fluxplane's
`fluxplane-plugin skill`. Keeps the agent's view of available integrations, their ops, inputs, and auth in
sync with what is installed, with no hand-maintained catalog.

## Acceptance
- [x] `flux-markdown` gains a frontmatter **writer** (`compose_frontmatter` + `render_document`), re-exported
      from `lib.rs`. Test `render_round_trips_through_parse`: `parse_frontmatter(render_document(&m, b)) == (m, b)`.
- [x] `flux plugin skill` discovers descriptors, fetches each `manifest()`, and renders a single
      `flux-plugin` skill + one `references/<plugin>.md` per plugin. Default prints to stdout; `--out <file>`
      writes there; `--install` writes `<cwd>/.flux/skills/flux-plugin/`; `--global` writes
      `~/.claude/skills/flux-plugin/`; re-running `--install` regenerates.
- [x] The render is a pure `fn(&[(name, PluginManifest)]) -> RenderedSkill`. Tests
      `frontmatter_round_trips_as_claude_format` + `one_reference_per_plugin_with_ops_and_required_input`
      + `empty_install_is_handled`.
- [x] Manual e2e: `flux plugin install plugins/target/debug` → `flux plugin skill` prints the SKILL.md; `skill
      --install --global` wrote the tree; gitlab/prometheus references rendered op tables + auth + endpoints.
- [x] Gate green (workspace test/clippy/fmt + codegate: flux-markdown stays L0).

## Progress
- **Code-complete + gate-green, pending commit.** `flux-markdown` writer (`compose_frontmatter`/
  `render_document`) + `crates/flux-cli/src/plugin_skill.rs` (pure renderer, 3 unit tests) +
  `PluginAction::Skill` / `run_plugin_skill` in `main.rs`. Added `serde` as a direct dep of flux-cli (was
  transitive). E2e verified against the installed pack. Out of scope (deferred): a cross-invocation
  `flux plugin blob`-style skill cache; progressive `references/` auto-load (L-02).

## Notes
- Design: [plugin-skill-generation.md](../designs/plugin-skill-generation.md). Epic:
  [fluxplane-plugins-parity.md](../designs/fluxplane-plugins-parity.md).
- Touch points: `crates/flux-markdown/src/frontmatter.rs` (+ `lib.rs`), `crates/flux-cli/src/main.rs`
  (`PluginAction`, `run_plugin`).
- Reuse: the `discover`/`PluginHost::spawn`/`manifest` path from the `Call`/`Install` arms;
  `flux_skill::default_skill_dirs`; `serde_norway`. Keep `SKILL.md` compact (24 KB / per-skill activation cap),
  push op detail into `references/`. Out of scope: a general `flux skill` surface; auto-regeneration on
  install; progressive `references/` auto-load (that is L-02).
