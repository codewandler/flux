# Claude interop — commands + skills that load from both worlds

**Status:** IMPLEMENTED (designed + built 2026-07-28; full workspace gate green, unreleased) · **Pillar:** Agent · **Stories:** D-186 · D-187 · D-188 · D-189 · D-190 · D-191 · D-192 — all done (supersedes C-93)

## Why

A growing share of users arrive with a `~/.claude` directory full of skills and slash commands.
Flux already half-speaks that dialect: skill discovery walks `.claude/skills` and `~/.claude/skills`
alongside the `.flux` trees, and `SKILL.md` frontmatter (`name`, `description`) parses fine. But the
compatibility story was never examined end-to-end, and an audit (2026-07-28) found the surface is
honest only by accident:

- **Command files don't exist.** `.claude/commands/*.md`, `.flux/commands`, and their `~` twins are
  never read; slash commands are hardcoded human-only built-ins in `flux-cli`'s REPL
  (`crates/flux-cli/src/session.rs`) and the TUI (`crates/flux-tui/src/lib.rs`). No `$ARGUMENTS`/`$1`
  substitution, no `argument-hint`.
- **Skills are load-compatible but semantics-incompatible.** Activation is manual-only (`--skill`);
  Claude's progressive disclosure (surface every name+description, let the model load bodies on
  demand) has no counterpart, so a directory of Claude skills does nothing until each is named.
- **Unknown frontmatter is silently dropped.** `allowed-tools`, `model`, `context`, `hooks`,
  `disable-model-invocation`, `argument-hint` all vanish in serde with no warning;
  `flux_skill::validate()` implements the Agent Skills naming rules but is never called.
- **Multi-file skills degrade.** Only `SKILL.md` is read; `references/` and scripts are neither
  loaded nor path-disclosed, so the model has no anchor to reach them. `Skill.source` is captured
  but never surfaced. Discovery is one level deep, so namespaced skill trees are invisible.
- **The crate has drifted.** `flux-skill`'s lazy/progressive loader, `active_for` trigger ranking,
  and `validate` are dead code; the production path (`flux-runtime::metadata`) reads bodies eagerly,
  contradicting `flux-skill`'s own crate docs.

The organizing idea: **be compatible where Claude's semantics are good, stay deliberately divergent
where ours are better, and be loud about the difference.** Manual skill activation is a measured win
(18% token reduction, `docs/designs/manual-skill-activation.md`) and stays the default; silent
field-dropping is not a design decision, it's a bug.

## Approach

Seven stories, roughly in dependency order:

1. **D-186 — Command files, human invocation.** Discover `*.md` command files from project
   `.flux/commands` + `.claude/commands` and user `~/.flux/commands` + `~/.claude/commands` with the
   same first-wins precedence and symlink jail as skills (reuse
   `flux-runtime::metadata` discovery + `flux-system` nested readers). `$ARGUMENTS` and `$1..$9`
   substitution; frontmatter `description` + `argument-hint` shown in REPL/TUI `/help` and the slash
   menu. File-based commands must not shadow built-ins.
2. **D-187 — Agent-invocable commands & skills** (absorbs C-93, keeping its three-gate contract:
   permitted ∧ accessible ∧ explicitly agent-triggerable, fail-closed, through `Executor::dispatch`,
   frozen `TurnIdentity`).
3. **D-188 — Opt-in model-invoked skills.** An opt-in mode (flag/config) that surfaces every
   discovered skill's name+description and lets the model pull a body on demand; honors
   `disable-model-invocation`. Manual `--skill` stays the default; this is progressive disclosure
   for those who want Claude's ergonomics and accept the token cost.
4. **D-189 — Frontmatter honesty.** Wire `flux_skill::validate()` into discovery as a lint; warn
   (once, at load) on recognized-but-unsupported fields instead of silently dropping them; honor
   `allowed-tools` (→ op allowlist for the skill-activated turn surface) and `model` where flux has
   real equivalents. Document the rest as explicitly unsupported.
5. **D-190 — Supporting-file disclosure.** Carry `Skill.source` through to the injected `<skill>`
   block (a `path=` attribute) so the model can `read` sibling `references/` files; no eager loading.
6. **D-191 — Nested skill discovery.** Recurse namespaced trees (`.claude/skills/<ns>/<name>/SKILL.md`)
   with dedup and jail semantics preserved.
7. **D-192 — flux-skill reconciliation.** Delete-or-align the dead lazy loader, `active_for`
   ranking, and stale crate docs against the production `flux-runtime::metadata` path; one
   discovery implementation, honestly documented.

Docs land with the epic's first slice, not at the end: a dedicated
`website/docs/agent/claude-compat.md` page states what loads from where, the supported/ignored
frontmatter matrix, and the deliberate semantic deltas — scoped to what actually ships, updated as
each story lands.

## Alternatives considered

- **Full Claude semantics (model-invocation by default).** Rejected: manual activation is a
  measured token win and flux's "the LLM is not the runtime" stance; auto-surfacing everything
  re-introduces the cost we deliberately removed. Opt-in keeps both audiences.
- **Ignore `.claude/commands`, invent only `.flux/commands`.** Rejected: the point is that existing
  Claude setups work in place; a flux-only tree helps nobody migrating.
- **Eagerly load `references/` into the prompt.** Rejected: unbounded token cost; path disclosure +
  the existing `read` op gets the same capability lazily.
- **Honor `hooks` / plugin-bundled `plugin:skill` namespacing.** Deferred as non-goals (v1): hooks
  are a harness-lifecycle contract flux doesn't have; plugin skill bundling belongs to the plugin
  distribution surface.

## Risks & open questions

- `allowed-tools` maps to Claude tool names (`Bash`, `Edit`, …), not flux op names — D-189 needs a
  translation table and a decision for unmappable entries (warn + ignore is the default stance).
  **Resolved (D-189):** an explicit `(&'static str, &'static str)` table in `flux-skill`
  (`Bash`→`bash`, `Edit`→`edit`, `Read`→`read`, `Grep`→`grep`, `Glob`→`glob`, `Write`→`write`,
  `WebFetch`→`web.fetch`, `WebSearch`→`web.search`, `Task`→`task`); an unmapped entry warns and is
  dropped, never guessed at. The translated set narrows the turn's advertised ops
  (`FlowEngine::narrow_by_skill_allowed_tools`, applied after group/policy gating) — narrowing
  only, same spirit as a role's `tools:` allowlist, never widening.
- **Resolved (D-189): skill `model` vs. an explicit CLI/SDK model.** A skill's `model` is a
  precedence tier between the caller's explicit choice and the config/default fallback:
  `--model`/SDK explicit > skill `model` > config `model` > `sonnet`. This mirrors `Role::to_spec`'s
  `model.unwrap_or(default_model)` — the skill sits where a role's own `model` sits, just one tier
  below the caller instead of unconditionally applied. Skills load before model resolution in
  `flux-cli::execution::build_agent_with` specifically so the skill's `model` can take part in that
  chain before the primary provider is built. If several enabled skills declare a `model`, the
  first one (by `--skill` order) wins — a corner case worth naming, not one that needed a richer
  merge rule.
- D-188's opt-in mode needs a mechanism decision: reuse D-187's `command.invoke` op, or a dedicated
  one? And does a loaded skill persist for later turns, or only the turn that requested it?
  **Resolved (D-188):** a dedicated op, `skill.load(name)` in `flux-tools`, not a reuse of
  `command.invoke` — the two gate on genuinely different things (D-187 gates on
  `agent-triggerable` + policy + discovery for *any* command/skill; `skill.load` gates only on
  "is this catalog non-empty", and conflating them would make `disable-model-invocation` and
  `agent-triggerable` look like the same axis when D-187's resolution above is explicit that they
  aren't). `skill.load` is **unconditionally registered** in `try_register_builtins` (same stance
  as `observe`/`evidence` — registry presence is not exposure) but only ever **advertised** when a
  new `FlowEngine::narrow_by_skill_catalog` step finds a non-empty opt-in catalog for the engine,
  which is empty unless a caller explicitly opts in — this is what keeps
  `skills_are_disabled_until_named_explicitly` byte-identical with the opt-in untouched. The
  catalog itself is discovered once by the caller (CLI `--skills-model-invoked` / `[skills]
  model_invoked`, or SDK `ClientBuilder::model_invoked_skills()` / `AgentSpec::
  try_with_model_invoked_skills()`), filtered to exclude `disable-model-invocation: true` skills,
  and handed to the engine via `FlowEngine::with_model_invoked_skills` — stored on the long-lived
  `EngineLoopHost` (not the engine struct itself) because that is the same object installed as the
  new `flux_runtime::SkillLoader` capability on `ToolContext`, exactly mirroring how
  `CompositeRegistrar`/`op.register` already cross the tool↔engine boundary. **Persistence:
  yes** — a loaded skill is recorded per-session on the loop host
  (`EngineLoopHost::loaded_skill_names`) and re-injected as a full `<skill>` body on every
  subsequent turn of that session, exactly like an explicitly `--skill`-activated one; the same
  `skill.activated` observation fires either way, so an audit trail can't (and isn't meant to)
  distinguish manual from model-invoked activation after the fact — one consistent semantics
  regardless of which path turned a skill on. The system prompt additionally gains a compact
  `<available-skills>` block (name + description + disclosed `path` when D-190 has one) whenever
  the catalog is non-empty, listing every catalog entry unconditionally (including already-loaded
  ones — re-listing costs a line and `skill.load` is idempotent, so there's no correctness reason
  to track and exclude them).
- Agent-invocable commands (D-187) inherit C-93's open design question: does the agent invoke the
  command's *effect* or a narrower capability? Design-first, per the original story.
  **Resolved (D-187):** the narrower capability, deliberately. A guarded op `command.invoke(kind:
  "command" | "skill", name, arguments?)` in `flux-tools` runs through `Executor::dispatch` under
  the turn's frozen `TurnIdentity`, gated by three **independently enforced, fail-closed** checks
  that any of C-93's four test cases can flip on its own: **(1) permitted** — a named-operation
  `AuthorityRequirement` (action `command.invoke`, resource `Operation:{kind}:{name}`) must be
  policy-granted for this exact target, checked entirely by the shared dispatch envelope before
  `execute` runs, not by the op itself; **(2) accessible** — `execute` re-runs the same guarded
  discovery (`flux_runtime::metadata::discover_commands` / `discover_skills`) that
  `flux_runtime::detect_signals` already used to raise the op's own evidence signal, so the two can
  never disagree about what is "discovered in this session" (no separate session registry to keep
  in sync); **(3) agent-triggerable** — a new frontmatter key `agent-triggerable: true` (default
  `false`) parsed onto `CommandFile` (`flux-runtime::metadata`) and `Skill`
  (`flux-skill::SkillFrontmatter`), silently, alongside D-188's `disable-model-invocation` and
  D-189's `allowed-tools`/`model` — a *separate* axis from both: `disable-model-invocation` governs
  passive surfacing in D-188's opt-in catalog, `agent-triggerable` governs active invocation here,
  and a target can be either, neither, or both independently. Invoking `kind: "command"` expands
  `$ARGUMENTS`/`$1..$9` (the existing `expand_command_arguments`) and returns the substituted body
  as the op's `ToolResult` text — prompt material for the model's current turn, not a nested turn
  and not the body's execution. Invoking `kind: "skill"` returns the body verbatim (equivalent to
  `read`ing it). Any missing gate degrades to a clean, recoverable `ToolResult::error` — never a
  hard `Err`, never partial execution. The op is evidence-gated on a new group `agent_invoke`
  (`groups.rs`), surfaced only when `detect_signals` finds at least one on-disk agent-triggerable
  command or skill — an ordinary project with no opted-in target never sees `command.invoke` in its
  catalog. C-93's four-way gate matrix (triggerable+permitted+accessible runs; human-only refused;
  inaccessible refused; policy-denied refused) is a direct unit-test suite against this op with no
  further design translation needed.
- Command-file `!`-prefixed inline bash and `@file` references (Claude features) are not in scope
  for D-186; decide during design whether to warn or pass through as literal text.
- Precedence when the same name exists as both a flux built-in and a file command: built-in wins,
  file command reachable under a disambiguated name, or hard error? D-186 design decides; built-in-wins
  is the working assumption.

## Acceptance / done

- A directory of real Claude Code assets (`.claude/skills/**` incl. nested + `references/`,
  `.claude/commands/*.md` with `$ARGUMENTS`) works in flux: commands invocable from REPL/TUI (and by
  the agent when triple-gated), skills load with a lint report instead of silent drops, model-invoked
  skills available behind the opt-in.
- No silently dropped frontmatter: every recognized-unsupported field warns; the docs matrix and the
  code agree (contract-tested where feasible).
- `website/docs/agent/claude-compat.md` exists, is linked from `skills-and-roles.md` and
  `claude-code.md`, and claims only shipped behavior.
- C-93 is closed as superseded by D-187 with a pointer.
- Union of D-186…D-192 acceptance; each behavioral change ships with a failing-first test.
