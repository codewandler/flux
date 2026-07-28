---
id: C-93
title: Let an agent invoke registered commands and skills when permitted, accessible, and agent-triggerable
pillar: Core
status: done
priority:
note: "SUPERSEDED by D-187 (claude-interop epic) — contract carried over unchanged, extended to file-based commands"
---

# Let an agent invoke registered commands and skills when permitted, accessible, and agent-triggerable

## Goal
Give the agent loop a guarded way to invoke a registered slash command or skill mid-turn — but only
when three gates all pass: the caller's policy **permits** it, the command/skill is **accessible**
in the current session (registered/discovered, and its tool group is active), and the command/skill
is explicitly marked **agent-triggerable**. Today commands and skills are human-only entry points at
the REPL/TUI; an agent cannot reuse them even when it would be safe and useful. This serves Core's
"the LLM is not the runtime" thesis — the invocation must still traverse authorization → approval →
guarded IO like any other effect, never a bypass path.

## Acceptance
- [ ] A command/skill declares whether it is **agent-triggerable** (default **false** — human-only
      stays the safe default; opting in is explicit). Skills already carry `triggers:` frontmatter
      (`flux_skill`), but discovery-time triggers are inert compatibility data — this flag is a
      separate, explicit authorization to let the agent *invoke*, not just match.
- [ ] A guarded op lets the agent invoke an agent-triggerable command/skill by name with arguments,
      routed through `Executor::dispatch` (policy + approval + redaction), with accurate
      `effects`/`permission_subjects`/`intents`. It is refused cleanly (recoverable ToolResult error)
      when the target is unknown, not agent-triggerable, or not accessible in this session.
- [ ] The three gates are enforced independently and fail closed: **permitted** (policy grant present),
      **accessible** (registered/discovered + owning tool group active), **agent-triggerable** (the
      explicit flag). Missing any one → refused, never executed.
- [ ] Caller identity is preserved: an agent-invoked command/skill runs under the same frozen
      `TurnIdentity` as the turn that triggered it — no identity swap, no privilege escalation.
- [ ] The op is only surfaced when its owning group's signal is detected (like other grouped tools),
      so ordinary turns without any agent-triggerable target don't see it.
- [ ] A failing-first test proves each gate: (a) an agent-triggerable+permitted+accessible target
      runs; (b) a human-only target is refused; (c) a permitted+triggerable but inaccessible target is
      refused; (d) a triggerable+accessible but policy-denied target is refused.
- [ ] Catalog/docs kept in sync: `crates/flux-flow/docs/ops-reference.md`, the relevant tool group in
      `groups.rs` (+ the `builtins_register` expected-name list), and any skill/command authoring docs
      that must mention the agent-triggerable flag.

## Progress
- 2026-07-28 — superseded by [D-187](D-187-agent-invocable-commands-skills.md) in the
  `claude-interop` epic ([design](../designs/claude-interop.md)): the three-gate contract here
  carries over verbatim and gains file-based commands (D-186) as targets. No work happened under
  this ID; closed to avoid a duplicate backlog row.

## Notes
- Distinguish the three concepts precisely: **slash commands** are surface-level REPL/TUI entries
  (`crates/flux-cli/src/session.rs`, `crates/flux-tui/src/lib.rs`) — some are pure display toggles,
  others mutate session state; **skills** are `.md`/`SKILL.md` docs discovered by `flux-skill`
  (project + `~/.flux/skills`, `~/.agents/skills`, `~/.claude/skills`). Not every command/skill
  should ever be agent-invokable — the explicit flag is the guard.
- Design-first: decide the invocation contract (does the agent call a slash command's *effect*, or a
  narrower agent-facing capability?), where the agent-triggerable flag lives for each of the two
  kinds, and how "accessible" is computed from the live session (registry + active groups). Put this
  in `docs/designs/` before implementing.
- Invariant: no bypass path. Any agent-driven command/skill invocation must go through
  `Executor::dispatch` and the same approval/guarded-IO envelope; the model never authors executable
  Flux. Caller identity stays frozen for the turn (`TurnIdentity`).
- Non-goal: auto-activating skills by their discovery `triggers:` — those stay inert compatibility
  data until a measured router justifies enabling them (per AGENTS.md). This story is about *explicit*
  agent invocation, not implicit trigger-matching.
- Relevant surface: `flux-tools` (op spec + `permission_subjects` + `intents` + `execute`, IO via
  `ctx.system`), the tool groups in `groups.rs`, `flux-skill` (skill discovery/frontmatter), and the
  slash-command dispatch in `flux-cli`/`flux-tui`.
