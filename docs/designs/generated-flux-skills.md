# Generated Flux skills

Flux ships generated Claude-format skills so agents can load focused Flux knowledge without treating
hand-written docs as catalogs. The command surface is:

```text
flux skill [cli|lang|plugin|ops] [--install] [--global]
```

With no type, `flux skill` renders the root index skill. Without `--install`, commands print the
selected `SKILL.md` to stdout. With `--install`, Flux writes skill directories.

## Install layout

Project install writes under `.flux/skills`; global install writes under `~/.claude/skills`.

```text
flux/
flux-cli/
flux-lang/
flux-plugin/
flux-ops/
```

`flux skill --install` writes the root plus every section. `flux skill <type> --install` writes the
root plus that one section. The generated directories are refreshed atomically enough for CLI use:
Flux removes each generated skill directory before rewriting it, so stale plugin references do not
survive a refresh.

Flux skill discovery loads project-local `.flux/skills` first, then project-local `.claude/skills`,
then user-global `~/.flux/skills`, `~/.agents/skills`, and `~/.claude/skills`. Project `.flux/skills`
wins on name collisions.

## Sources of truth

The generated skills do not scrape docs:

- `flux skill cli` renders from the Clap command tree (`Cli::command()`), so command names, flags, and
  help text come from the parser that runs the binary.
- `flux skill lang` rewraps the generated Flux-Lang skill from `flux_lang::skill::render()`, which
  already derives node/prelude tables from the language schema/doc-comments.
- `flux skill ops` builds a lightweight `ToolRegistry`, adapts it through `flux_flow::registry::OpRegistry`,
  and renders operation signatures plus `ToolSpec` metadata. Group surfacing comes from the same
  built-in/eval group declarations and local group config the agent uses.
- `flux skill plugin` renders installed plugin manifests. It spawns plugins only to fetch manifests,
  never to call operations; failures are skipped with a note so one broken plugin does not block the
  catalog.

The legacy `flux plugin skill` command remains as an alias for the plugin section renderer.
