---
title: Claude Code compatibility
description: "What flux loads from a .claude directory today — skill locations and frontmatter, what is honored vs ignored, and where the semantics deliberately differ."
---

# Claude Code compatibility

Flux reads Claude Code's on-disk formats where that helps you reuse an existing setup, and keeps its
own semantics where they are a deliberate improvement. This page is the honest matrix: what loads
from where, which metadata is honored, and where behavior differs from Claude Code. It describes
what ships today; planned compatibility work is tracked in the repo roadmap under the
*Claude interop* epic.

> This page is about **file-format and harness compatibility**. For running flux *on a Claude
> subscription* (the `claude` provider), see [Claude Code (subscription)](./claude-code.md).

## Skills: what loads from where

Skill discovery walks both dialects' directories, highest precedence first:

1. repeatable `--skill-dir` entries, then `[skills] dirs` from config;
2. project `.flux/skills`;
3. project `.claude/skills`;
4. `~/.flux/skills`;
5. `~/.agents/skills`;
6. `~/.claude/skills`.

Earlier directories win when two skills share a name — a namespaced duplicate (`<ns-a>/foo` and
`<ns-b>/foo` both naming `foo`) resolves the same way, and the shadowed one logs a warning. Each
root may contain flat `*.md` files (top level only) plus a `SKILL.md` at any depth below it, up to
4 directory levels — the one-level `<name>/SKILL.md` shape Claude Code uses and deeper namespaced
trees (`.claude/skills/<group>/<name>/SKILL.md`) both resolve. A directory that directly contains
`SKILL.md` claims its whole subtree: traversal does not descend past it, so a skill's own supporting
directories (e.g. `references/`) never surface as separate skills.

Project-local skill directories are confined to the workspace: symlinks that escape it are
rejected, at any nesting depth. User-level and explicitly configured directories are trusted as-is.

## Skill frontmatter: honored vs warned vs ignored

| Field | Status in flux |
|---|---|
| `name` | Honored. Falls back to the file stem or skill directory name. |
| `description` | Honored (shown in listings; not auto-surfaced to the model). |
| `triggers` (flux extension) | Parsed; inert compatibility metadata — never auto-activates. |
| `allowed-tools` | **Honored.** Claude tool names (`Bash`, `Edit`, `Read`, `Grep`, `Glob`, `Write`, `WebFetch`, `WebSearch`, `Task`, …) translate to flux op names via an explicit table. While the skill is active (enabled via `--skill`), the turn's surfaced ops are narrowed to this allowlist — narrowing only, layered on top of whatever policy/group gating already produced, never widening it. An entry with no flux equivalent warns at load and is dropped. |
| `model` | **Honored.** A model override for turns where the skill is active, resolved the same way a role's `model` is: it fills in only when nothing higher-precedence set one. Precedence is `--model`/SDK explicit choice > skill `model` > config `model` > the built-in default. If several enabled skills declare a `model`, the first one (by `--skill` order) wins. |
| `disable-model-invocation` | **Honored.** Excludes the skill from the [opt-in model-invoked surface](#model-invoked-skills-opt-in) — both the surfaced listing and `skill.load` — but never from manual `--skill` activation. |
| `argument-hint` | Parsed silently (no warning) onto the skill, for the agent-invocable-command surfaces to read. |
| `agent-triggerable` (flux extension) | **Honored.** Parsed silently, default `false`. When `true`, the agent may invoke this skill itself via `command.invoke` (see [Agent-side invocation](#agent-side-invocation) below) — a separate, explicit opt-in from `triggers`/`disable-model-invocation`, which govern surfacing and matching, not invocation. |
| `context`, `agent`, `hooks`, `license`, `compatibility` | **Recognized but unsupported.** Presence produces one load-time warning naming the skill and the field, instead of vanishing silently. |
| anything else | Truly unknown — silently ignored (there is nothing meaningful to warn about). |

Every load-time warning (an unsupported field, an unmappable `allowed-tools` entry, an invalid
`name`/`description` caught by the Agent Skills naming lint, or a shadowed duplicate) prints once at
skill load, prefixed `warning:` — never a silent drop for a field flux actually recognizes.

## Where the semantics deliberately differ

**Activation is manual by default, not model-driven.** Claude Code surfaces every skill's name and
description and lets the model load bodies on demand. Flux skills are inactive until you name them —
`flux run --skill rust-style "…"` — and then the full body is injected on every turn. This is a
deliberate divergence (measured ~18% token reduction on ordinary turns) and stays the default; an
*opt-in* progressive-disclosure mode matching Claude Code's ergonomics ships alongside it — see
[Model-invoked skills (opt-in)](#model-invoked-skills-opt-in) below.

### Model-invoked skills (opt-in)

Pass `--skills-model-invoked` (or set `[skills] model_invoked = true`, or call
`ClientBuilder::model_invoked_skills()` / `AgentSpec::try_with_model_invoked_skills()` on the SDK) to
get Claude Code's progressive disclosure instead of the manual default. With it on:

- every discovered skill's `name` and `description` (plus its disclosed `path`, if any) are appended
  to the system prompt as a compact `<available-skills>` listing — bodies are **not** injected up
  front;
- a skill with `disable-model-invocation: true` is excluded from that listing entirely, and from
  loading — the same field Claude Code itself honors;
- the model pulls a skill's full body into context by calling `skill.load(name)`, which is only
  surfaced when the opt-in is on and at least one loadable skill was discovered;
- once loaded, a skill behaves exactly like an explicitly `--skill`-activated one for the rest of
  that session — its body is re-injected on later turns too, so there is one consistent semantics
  for "this skill is active" regardless of which path activated it.

The opt-in is additive to, not a replacement for, `--skill`/`AgentSpec.skills`: both can be used
together, and a manually-activated skill's body is always injected regardless of the catalog.
Leaving the opt-in off is byte-identical to skills not existing at all — this is what
`skills_are_disabled_until_named_explicitly` pins.

**Supporting files are disclosed, not loaded.** A skill's body is always injected, and alongside it
flux discloses *where the skill lives* — the injected tag carries a `path` attribute, e.g.
`<skill name="pkg" path="/project/.claude/skills/pkg">`, pointing at the skill's directory for a
`SKILL.md` layout or at the file itself for a flat `<name>.md` skill. This is an anchor, not a
preload: `references/`, scripts, and templates next to `SKILL.md` are never read into the prompt —
if the body says "see `references/extra.md`", the model reaches it with an ordinary `read` op
against the disclosed path, at whatever cost that read normally has. A skill with no on-disk
location (constructed in-memory, e.g. by the SDK) carries no `path` attribute — there is nothing to
disclose. Disclosure never widens access: the read still goes through the standard
authorization/approval flow, so a project-local skill's files are reachable because they already
sit inside the workspace jail, and a `~`-trusted user-global skill's files follow whatever the
normal policy decides for reads outside the project.

## Slash commands

Flux discovers Markdown command files from four directories — the skill roots above minus
`~/.agents/skills`, which has no command counterpart — highest precedence first:

1. project `.flux/commands`;
2. project `.claude/commands`;
3. `~/.flux/commands`;
4. `~/.claude/commands`.

Each `*.md` file is one command named after its file stem (`review.md` → `/review`), flat only — no
namespaced subdirectories. Earlier directories win a name clash, same as skills; project directories
are symlink-jailed to the workspace, user-global directories are trusted as-is. A command file named
after a **built-in** (`/help`, `/model`, `/clear`, …) is dropped at load with a warning — built-ins
always win.

Frontmatter `description` and `argument-hint` are honored and shown in `/help` and the REPL/TUI
slash menu; `agent-triggerable: true` (flux extension, default `false`) opts the command into
agent-side invocation — see [Agent-side invocation](#agent-side-invocation) below; any other
frontmatter field is ignored with a load-time warning naming the file and the field (not silently
dropped — full frontmatter linting is separate follow-up work).

Typing `/name args…` at the REPL or in the TUI substitutes `$ARGUMENTS` (the full trailing text) and
`$1`..`$9` (whitespace-split positional arguments; a missing positional substitutes empty) into the
command body, then runs the result as the turn's prompt — exactly as if you had typed the
substituted text yourself.

```markdown title=".flux/commands/review.md"
---
description: Review a PR for style and correctness
argument-hint: <pr-number>
---
Review PR #$1 for style and correctness issues.
```

`/review 42` runs the turn `Review PR #42 for style and correctness issues.`

**Not interpreted:** Claude's `!`-prefixed inline-bash (`` !`git status` ``) and `@file` references
in a command body pass through as literal text — flux does not execute or expand them. A command
file relying on either behaves differently in flux than in Claude Code.

### Agent-side invocation

Command files and skills are dispatched from the REPL/TUI by a human by default. The agent itself
can invoke one mid-turn through the `command.invoke` op, but only when **three independently
enforced, fail-closed gates** all pass — missing any one is a clean refusal, never a partial
invocation:

1. **permitted** — your policy grants the `command.invoke` operation for this exact target;
2. **accessible** — the target is discovered in the current session (same discovery this page
   documents above, re-run at call time);
3. **agent-triggerable** — the target's own frontmatter opts in explicitly
   (`agent-triggerable: true`, default `false`). Most commands and skills stay human-only; this is
   a per-target, explicit decision, not a global switch.

`command.invoke(kind: "command" | "skill", name, arguments?)` expands `$ARGUMENTS`/`$1..$9` for a
command and returns the substituted body as prompt text for the model's current turn — it does not
execute the body or start a nested turn. Invoking a skill returns its body, equivalent to reading
it. The call runs under the turn's frozen caller identity like any other op: no bypass, no
escalation. `command.invoke` is itself evidence-gated — it is only surfaced to the model when the
session has discovered at least one agent-triggerable command or skill.

## Exporting in Claude's format

The compatibility also runs outward: `flux skill [cli|lang|plugin|ops] --install --global` writes a
generated, Claude-format skill (frontmatter `name` + `description`, plus `references/`) into
`~/.claude/skills/`, where Claude Code and flux can both discover it.

## Related docs

- [Skills and roles](./skills-and-roles.md) — authoring skills, activation, sub-agent roles.
- [Claude Code (subscription)](./claude-code.md) — the `claude` provider and model aliases.
- [Configuration](../reference/config.md) — `[skills] dirs`.
- [CLI reference](./cli.md) — the built-in slash commands.
