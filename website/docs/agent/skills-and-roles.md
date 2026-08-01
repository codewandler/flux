---
title: Skills and roles
description: "Teach an agent project knowledge with explicitly enabled skills and define capability-scoped sub-agent roles."
---

# Skills and roles

Skills and roles change different parts of an agent:

- A **skill** is contextual knowledge that is inactive until a human enables it or one of the
  documented model/agent invocation opt-ins loads it.
- A **role** defines a sub-agent persona, optional model override, and tool allow-list used by `task`.

Neither grants new host capabilities. Skill prose can guide a plan; role tools can only narrow the
parent's available operation set.

## Skills

A skill may be a Markdown file or a directory containing `SKILL.md`. Flux-native and Agent
Skills/Claude-compatible metadata is accepted:

```markdown
---
name: rust-style
description: Project conventions for Rust changes and verification.
triggers: [rust, cargo, clippy]
---

Use `cargo fmt --all` and keep clippy clean with warnings denied.
```

Discovery reads and parses the whole skill file, but discovery alone does not put its body in the
prompt. Skills are inactive by default: user-message keywords, names, descriptions, and `triggers`
do not activate one. Enable one or more by name:

```bash
flux run --skill rust-style "fix the parser"
flux run --skill rust-style --skill release-checks "prepare the release"
```

An unknown name fails before a model call. Once enabled, the skill body is injected on every turn
of that agent. `triggers` remain compatible metadata for tooling and a future evaluated router;
they are not an automatic activation mechanism.

A skill's supporting files — `references/*.md`, scripts, templates next to `SKILL.md` — are never
loaded into the prompt. Instead the injected `<skill>` tag discloses where the skill lives (its
directory for a `SKILL.md` skill, the file itself for a flat one), so a body that says "see
`references/extra.md`" gives the model an anchor to `read` it lazily, on demand, at ordinary `read`
cost — never eagerly, and never with a widened grant (the read still goes through the normal
authorization/approval flow). See [Claude Code compatibility](./claude-compat.md) for the details.

A skill may also carry Claude's `allowed-tools` and `model` — both honored:

```markdown
---
name: reviewer
description: Read-only review — no edits, no shell.
allowed-tools: Read, Grep, Glob
model: haiku
---

Review the diff for correctness and style. Do not modify files.
```

- `allowed-tools` narrows the turn's surfaced ops to the listed set while the skill is active
  (translated from Claude tool names to flux ops — `Bash`→`bash`, `Edit`→`edit`, `Read`→`read`,
  `Grep`→`grep`, `Glob`→`glob`, `Write`→`write`, `WebFetch`→`web.fetch`, `WebSearch`→`web.search`,
  `Task`→`task`). This narrows only — it sits on top of whatever policy/group gating already
  produced and never grants an op that wasn't already available. An entry with no flux equivalent
  warns at load and is dropped.
- `model` overrides the turn's model while the skill is active, but an explicit `--model` (or SDK
  model choice) always wins — the skill only fills in when nothing more specific set one, the same
  precedence a role's `model` resolves against its parent.
- Any other Claude field flux recognizes but doesn't implement (`context`, `agent`, `hooks`,
  `license`, `compatibility`) warns once at load, naming the skill and the field, instead of
  vanishing silently.
- `agent-triggerable: true` (flux extension, default `false`) lets the *agent itself* invoke this
  skill mid-turn via the guarded `command.invoke` op, on top of policy and session-discovery
  gates — a human-activated skill (`--skill`) stays the default; this flag is a separate, explicit
  opt-in for agent-side invocation. See
  [Agent-side invocation](./claude-compat.md#agent-side-invocation).

### Model-invoked skills (opt-in)

By default, flux does not advertise the general discovered-skill catalog to the model; explicit
`--skill`/`AgentSpec.skills` activation is the baseline. The narrower `agent-triggerable: true` path
described above remains a separate per-skill opt-in. If you want Claude Code's progressive-disclosure
ergonomics—surface every eligible skill's name and description, then let the model load a body on
demand—and accept the extra tokens, opt in:

```bash
flux run --skills-model-invoked "find and use the right skill for this"
```

or in `.flux/config.toml`:

```toml
[skills]
model_invoked = true
```

or from the SDK:

```rust
// ClientBuilder
Client::builder().model_invoked_skills().build(provider, ".")?;

// AgentSpec (set `cwd` first — discovery is rooted there)
AgentSpec { cwd: root, ..AgentSpec::new("sonnet") }.try_with_model_invoked_skills()?;
```

With the opt-in on, every discovered skill except one marked `disable-model-invocation: true` gets
its name+description appended to the system prompt as a compact listing; the model pulls a body into
context by calling `skill.load(name)`, which only appears in the op catalog when the opt-in is on and
at least one loadable skill exists. A loaded skill then behaves exactly like an explicitly `--skill`
one for the rest of the session — its full body is re-injected on every later turn too. This is
additive to `--skill`/`AgentSpec.skills`, not a replacement: both can be active at once. See
[Model-invoked skills (opt-in)](./claude-compat.md#model-invoked-skills-opt-in) for the full
semantics.

Default precedence, highest first:

1. project `.flux/skills`;
2. project `.claude/skills`;
3. `~/.flux/skills`;
4. `~/.agents/skills`;
5. `~/.claude/skills`.

Repeatable `--skill-dir` entries precede all defaults. `[skills] dirs` comes next, with project
entries before user entries. Earlier directories win when two skills share a name. These options
change discovery only; use `--skill` to activate a discovered name.

`flux skill [cli|lang|plugin|ops]` prints a generated skill; add `--install` for project
`.flux/skills`, or `--install --global` for `~/.claude/skills`. Installation does not activate it;
pass `--skill <name>` when you want it. `flux plugin skill` generates the operation reference for
installed plugins.

## Sub-agent roles

Put roles in `.flux/agents/<name>.md` in the project or `~/.flux/agents/<name>.md` for user-wide
reuse. The filename stem is the default role name:

```markdown
---
description: Read-only repository reconnaissance
model: haiku
thinking: true
effort: low
tools: [read, glob, grep, git_status, git_diff]
---

Inspect the requested area quickly. Do not modify files. Return evidence with paths.
```

- Omit `model` to inherit the parent's model. A role's `model` resolves against the parent's
  provider aliases.
- Omit `thinking` and `effort` to inherit the parent's reasoning policy. Set them explicitly to
  override it for that role (`effort`: `low`, `medium`, `high`, `xhigh`, or `max`).
- Omit `tools` to inherit the tools available to the parent.
- Use `tools: []` to grant no operations.
- A listed tool is still subject to policy, approval, and the parent capability floor.

The built-in roles (`scout`, `planner`, `worker`, `reviewer`, `evaluator`, and `summarizer`) are used
when no file defines that name. Delegate with `task({role: "scout", task: "map the parser"})`.

## Related docs

- [Claude Code compatibility](./claude-compat.md) — which Claude skill metadata is honored vs ignored.
- [Configuration](../reference/config.md) — custom skill directories.
- [Safety & approvals](./safety.md) — capability narrowing for sub-agents.
- [Operations](../language/ops.md) — the `task` operation.
- [Multi-agent programs](./programs.md) — file-defined application agents and journeys.
