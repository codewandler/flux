---
title: Project context
description: "What flux tells the agent about your repository before the first turn — conventions files, environment and git state, and path-scoped guidance fragments."
---

# Project context

Before your first message, flux assembles a picture of the exact directory where the session starts
and folds it into the agent's system prompt, so the agent isn't cold-starting. That picture comes
from an ordered chain of providers:

| Source | What it contributes |
|---|---|
| Environment | Working directory and OS |
| Git | Branch, `git status --short`, recent commits, unstaged diff stat |
| Repo shape | Detected stack(s) and a top-level listing |
| Conventions files | `CLAUDE.md`, `AGENTS.md`, `.flux/context.md` — read in full |
| Guidance fragments | `.flux/context.d/*.md` — read only when they apply (see below) |

Everything here is rooted at the launch/workspace directory. Flux does not walk upward to find a
parent repository root or parent conventions file. Git itself may still recognize that the directory
is inside a larger worktree, but repo-shape inspection, conventions files, and guidance-fragment
discovery stay at the directory you launched from. Widening the agent's tool paths (`--add-dir`,
`--allow-all-paths`) deliberately does **not** widen what project context can read.

:::info Assembled once per session
The whole block is built at startup and stays fixed for the session, which is what keeps it in the
cache-stable part of the prompt. Editing a conventions file mid-session has no effect until you
restart.
:::

## Conventions files

Drop a `CLAUDE.md`, `AGENTS.md`, or `.flux/context.md` in the directory where you launch flux and its
full contents join the prompt. If those files live at the repository root, launch from that root;
starting in a nested directory does not discover the parent copies. This is the right place for rules
that apply to the whole workspace — code style, the test command, things never to do.

Because they are read in full on every session, they cost tokens whether or not they are relevant.
In a large repository that forces an unhappy choice: keep them short and lose subsystem detail, or
keep them complete and bury the relevant rule in noise.

Guidance fragments exist to remove that tradeoff.

## Guidance fragments

A **fragment** is a Markdown file in `.flux/context.d/` whose optional `globs:` frontmatter names
the paths it applies to. A fragment joins the prompt only when your **working set** — what
`git status` reports as changed — contains a matching path.

```markdown title=".flux/context.d/parser.md"
---
globs:
  - "crates/flux-lang/**"
---

Parser changes must keep the CST lossless: every token carries a range, and a parse error
produces an ERROR node rather than aborting.
```

With that file in place, the parser rules reach the agent when you are editing
`crates/flux-lang/src/parse.rs`, and stay out of the prompt when you are working on the TUI.

### Rules

- **No `globs:` means always load.** A fragment with no frontmatter, or with an empty glob list, is
  treated as unconditional — the same behavior as a conventions file, just kept in its own file.
- **Globs use the same syntax as policy path grants.** `*` spans `/`, so `crates/flux-lang/**`
  matches `crates/flux-lang/src/parse.rs`. `*.md` matches `README.md`.
- **Any glob matching any changed path is enough** for the fragment to load.
- **Untracked files count.** A brand-new file pulls in its subsystem's guidance.
- **Fragments load in filename order**, so the prompt is stable between runs.
- **Only `.md` files** in `.flux/context.d/` are considered. The directory is flat — subdirectories
  are not scanned, so one `ls` tells you everything that can load.

### When nothing matches

On a clean checkout, or outside a git repository, there is no working set — so scoped fragments
load nothing while unscoped fragments still load. Put anything the agent must always know in a
conventions file or an unscoped fragment; never rely on a scoped fragment for a rule that has to
hold everywhere.

### Errors are loud

A fragment that cannot be read, or whose frontmatter is not valid YAML, fails startup with the
filename in the message. Missing guidance is invisible at the point of use — flux would rather stop
than quietly drop a rule you believed was in effect.

One exception, by design: **symlinks are skipped silently**. flux never follows a symlink when
walking a directory, because following one could escape the workspace. A symlinked fragment is
therefore ignored — including one pointing somewhere harmless inside the repo. Use real files in
`.flux/context.d/`.

## Choosing where a rule goes

- Applies to the whole repo, always → `AGENTS.md` (or `CLAUDE.md` / `.flux/context.md`).
- Applies to one subsystem → a fragment with `globs:`.
- Reference material the agent should consult on demand rather than always carry → a
  [skill](./skills-and-roles.md) instead.

## Related docs

- [Context management](./context-management.md) — the other half: what happens to the *conversation*
  as it grows, and how compaction bounds it.
- [Skills and roles](./skills-and-roles.md) — the on-demand alternative to always-loaded context.
- [The agent loop](./agent-loop.md) — where the assembled block sits in a turn, and what runs after it.
- [Safety & approvals](./safety.md) — the authorization and guarded-IO envelope that reads these files.
- [Configuration](../reference/config.md) — the `[workspace]` table behind `--add-dir` and `--allow-all-paths`.
