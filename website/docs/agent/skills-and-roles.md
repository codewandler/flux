---
title: Skills and roles
description: "Teach an agent project knowledge with progressively loaded skills and define capability-scoped sub-agent roles."
---

# Skills and roles

Skills and roles change different parts of an agent:

- A **skill** is contextual knowledge injected when its name, description, or triggers match a turn.
- A **role** defines a sub-agent persona, optional model override, and tool allow-list used by `task`.

Neither grants new host capabilities. Skill prose can guide a plan; role tools can only narrow the
parent's available operation set.

## Skills

A skill may be a Markdown file or a directory containing `SKILL.md`. Flux-native skills can declare
explicit triggers; Agent Skills/Claude-compatible files can use `name` and `description` only:

```markdown
---
name: rust-style
description: Project conventions for Rust changes and verification.
triggers: [rust, cargo, clippy]
---

Use `cargo fmt --all` and keep clippy clean with warnings denied.
```

Discovery reads frontmatter first and loads a body only when that skill activates. At most four
skills and 24,000 body bytes are injected by default.

Default precedence, highest first:

1. project `.flux/skills`;
2. project `.claude/skills`;
3. `~/.flux/skills`;
4. `~/.agents/skills`;
5. `~/.claude/skills`.

Repeatable `--skill-dir` entries precede all defaults. `[skills] dirs` comes next, with project
entries before user entries. Earlier directories win when two skills share a name.

`flux skill [cli|lang|plugin|ops]` prints a generated skill; add `--install` for project
`.flux/skills`, or `--install --global` for `~/.claude/skills`. `flux plugin skill` generates the
operation reference for installed plugins.

## Sub-agent roles

Put roles in `.flux/agents/<name>.md` in the project or `~/.flux/agents/<name>.md` for user-wide
reuse. The filename stem is the default role name:

```markdown
---
description: Read-only repository reconnaissance
model: haiku
tools: [read, glob, grep, git_status, git_diff]
---

Inspect the requested area quickly. Do not modify files. Return evidence with paths.
```

- Omit `model` to inherit the parent's model. A role's `model` resolves against the parent's
  provider aliases.
- Omit `tools` to inherit the tools available to the parent.
- Use `tools: []` to grant no operations.
- A listed tool is still subject to policy, approval, and the parent capability floor.

The built-in roles (`scout`, `planner`, `worker`, `reviewer`, `evaluator`, and `summarizer`) are used
when no file defines that name. Delegate with `task({role: "scout", task: "map the parser"})`.

## Related docs

- [Configuration](../reference/config.md) — custom skill directories.
- [Safety & approvals](./safety.md) — capability narrowing for sub-agents.
- [Operations](../language/ops.md) — the `task` operation.
- [Multi-agent programs](./programs.md) — file-defined application agents and journeys.
