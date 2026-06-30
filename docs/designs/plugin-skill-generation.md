# Design: generated plugin skill — `flux plugin skill` (D-13)

**Status:** shipped; generalized by [generated-flux-skills.md](generated-flux-skills.md) ·
**Pillar:** Core · **Layer:** L6 (`flux-cli`) + L0 (`flux-markdown` writer) ·
**Owner:** Timo · **Story:** [D-13](../stories/D-13-plugin-skill-command.md) ·
**Epic:** [fluxplane-plugins-parity.md](fluxplane-plugins-parity.md)

## Why

fluxplane's `fluxplane-plugin skill` command *generates* a Claude-format `SKILL.md` + `references/<plugin>.md`
from the installed plugins' manifests (that is exactly what produced `~/.claude/skills/fluxplane-plugin/`). It
keeps the agent's view of "which integrations exist, their ops, inputs, and auth" in sync with what's actually
installed — no hand-maintained catalog. flux has everything to do the same: a `PluginManifest` per plugin
(ops + auth + endpoints + datasources), a skill format + loader (`flux-skill`), and a frontmatter parser
(`flux-markdown`). It is missing only a frontmatter **writer** and the CLI command. This is independent of the
D-12 protocol work and ships first.

## The flux-markdown writer (L0)

`flux-markdown` parses frontmatter but can't emit it. Add, symmetric with `parse_frontmatter`:

```rust
pub fn compose_frontmatter<T: Serialize>(meta: &T) -> Result<String, serde_norway::Error>; // the `---\n…\n---` block
pub fn render_document<T: Serialize>(meta: &T, body: &str) -> Result<String, serde_norway::Error>; // block + "\n" + body
```

Reuses `serde_norway` (already the frontmatter serde). Round-trip property:
`parse_frontmatter(render_document(&m, b)?) == Document { meta: m, body: b }` (modulo a trailing newline).

## The command — `flux plugin skill`

New `PluginAction::Skill { out: Option<PathBuf>, install: bool, global: bool }` under the existing
`flux plugin` tree (reuses the discovery + manifest-fetch path `Call`/`Install` already use):

1. `flux_plugin::discover(plugins_dir())` → for each descriptor, `PluginHost::spawn` + `manifest()`.
2. Render a single **`flux-plugin` skill** + one `references/<plugin>.md` per plugin.
3. Output target:
   - default (no flag): print `SKILL.md` to stdout (the references are summarized inline as a list).
   - `--out <file>`: write `SKILL.md` there (references next to it).
   - `install`: write the tree to `<cwd>/.flux/skills/flux-plugin/{SKILL.md,references/*.md}` — project-scoped,
     version-controlled, the **highest** precedence dir in `flux_skill::default_skill_dirs`.
   - `install --global`: write to `~/.claude/skills/flux-plugin/` instead (the user-global skill dir flux
     also scans).
4. `flux plugin skill refresh` ≡ `skill install` over the current install dir (idempotent regenerate).

### SKILL.md shape (kept compact — the activation cap is 24 KB / skill)

```markdown
---
name: flux-plugin
description: Call installed flux integration plugins (gitlab, slack, prometheus, …) via `flux plugin call`.
---
# Installed integration plugins
Use `flux plugin call <plugin> <op> '<json-input>'`. Inputs/auth per integration are in `references/`.
## Auth
Each plugin resolves secrets by purpose from env (listed per reference). Set the env vars, then call.
## Installed
- **gitlab** — GitLab projects/MRs/issues/pipelines. → references/gitlab.md
- **slack** — Slack messaging/search/channels/users. → references/slack.md
…
```

The current renderer emits Claude/Agent-Skills frontmatter (`name` + `description`, no `triggers`). Per-op
detail goes into `references/<plugin>.md` (an op table: name · description · required inputs · risk · auth
purpose), keeping the always-injected `SKILL.md` body small.

## Reuse, don't reinvent
- `flux_plugin::discover` / `PluginHost::spawn` / `manifest` — the same path `run_plugin`'s `Call`/`Install`
  arms use (`crates/flux-cli/src/main.rs`).
- `flux_skill::default_skill_dirs` for the install target precedence.
- `serde_norway` for frontmatter serialization (no new dep).
- `flux_markdown::render_document` (this design's L0 addition).

## Testing
- flux-markdown: `render_document` round-trips through `parse_frontmatter` (unit).
- renderer: a 2-fake-manifest fixture → `SKILL.md` whose frontmatter parses back with the expected
  Claude-format `name`/`description`, and one `references/<plugin>.md` per manifest containing each op name. Hermetic — no real
  subprocess (factor the render as a pure `fn(&[PluginManifest]) -> (Skill md, Vec<(name, md)>)`).

## Non-goals
- A general `flux skill` command surface was out of scope for D-13, then shipped later in
  [L-07](../stories/L-07-generated-flux-skills.md).
- Auto-regeneration on install/auth-change (fluxplane does this) — `refresh` is manual for now.
- Progressive-disclosure auto-loading of `references/` — that is [L-02](../stories/L-02-flux-markdown-engine.md);
  here the agent `read`s a reference on demand.
