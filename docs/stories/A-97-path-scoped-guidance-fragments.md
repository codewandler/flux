---
id: A-97
title: Path-scoped guidance fragments — load project conventions only when they apply
pillar: Agent
status: done
priority:
epic:
design:
note: "DONE — `.flux/context.d/*.md` with `globs:` frontmatter, scoped against the git working set and resolved ONCE at context assembly; the story's original per-turn premise was wrong (the system prompt is built once at startup, so per-turn scoping was both unimplementable and would have churned the cache prefix)"
---

# Path-scoped guidance fragments — load project conventions only when they apply

## Goal
Let a repository split its conventions into fragments that load **only when they apply**. Today
`ProjectFiles` reads `CLAUDE.md` / `AGENTS.md` / `.flux/context.md` in full every session
(`context.rs`), so a large repo either keeps its guidance thin (and the agent misses subsystem
rules) or pays the whole thing every time (and buries the relevant rule in noise). Path scoping
makes guidance grow with the repo without growing the prompt.

## Acceptance
- [x] A guidance fragment declaring `globs:` in frontmatter contributes its body **only** when the
      repository's working set contains a matching path — failing-first test with three fragments
      where exactly two fire (`fragments_load_only_when_globs_match_the_working_set`, drives a real
      `git init` repo because `git status --short` output is the parsed contract).
- [x] A fragment with no `globs` always contributes, so splitting a conventions file into pieces is
      lossless — covered by the same test plus `unscoped_fragments_survive_outside_a_repo`; the
      pre-existing `context.rs` tests pass unchanged.
- [x] ~~Fragments are referenced from the root guidance file~~ → **superseded:** fragments are
      discovered from a **flat `.flux/context.d/` directory**. The acceptance's real requirement was
      auditability ("what loads is knowable from one place"), and a flat directory satisfies it with
      one `ls` — no tree walk, and no need to invent a `@`-mention syntax the story itself flagged
      as undecided. It also matches how `.flux` already houses skills, agents, and flows. Subdirs
      are not scanned; only `.md` is considered.
- [x] The confinement invariant holds: fragments are read through a workspace pinned to the project
      root, never the agent's possibly-widened tool workspace. Pinned by
      `fragments_never_read_through_a_symlink`. Note the escape is **structurally impossible rather
      than rejected** — `System::walk_files` skips symlinks outright ("never follow symlinks (could
      escape a root)"), so a symlinked fragment is never read at all. That is quieter than
      `ProjectFiles`' symlink guard, which errors by name; the invariant pinned is the leak, not the
      diagnostic, and the website page documents the silent skip honestly.
- [x] The scoping signal is defined and documented: the **git working set**
      (`git status --short --untracked-files=all`), resolved once at context-assembly time.
      Documented in `website/docs/agent/project-context.md` and in the module docs.

## Progress
- **DONE 2026-07-28** on branch `feat/a97-path-scoped-guidance` (worktree
  `.claude/worktrees/a97-path-scoped-guidance`). Full gate green: `cargo build --workspace`,
  `cargo test --workspace` (128 suites, 0 failures), `cargo clippy --workspace --all-targets -D
  warnings` (exit 0), `cargo fmt --all --check` in **both** workspaces, `cargo test -p
  flux-codegate` (13 passed). Not committed — no instruction to commit.
- **The filed premise was wrong, and reading the code before writing any is what caught it.** The
  story specified scoping against "the turn's resolved path set". There is no such thing: the
  system prompt is assembled **once at CLI startup** (`crates/flux-cli/src/execution.rs`) into a
  `String` on `AgentSpec` (`crates/flux-agent/src/lib.rs`), and the agent loop has no per-turn
  context hook. Implementing the story literally would have meant moving context assembly into the
  turn loop — an architectural change to a hot path that would have taken guidance from
  frozen-per-session to variable-per-turn, i.e. newly *introducing* the cache-prefix churn the
  story's own Notes warned about and that C-133…C-140 had just finished eliminating. Re-scoped to
  the git working set (user decision) after presenting the three options.
- Two implementation findings worth keeping: `git status --short` **collapses untracked directories**
  to a bare `crates/` entry that no subsystem glob matches, so `--untracked-files=all` is required
  or a brand-new file silently fails to pull in its own guidance; and fragments must be **sorted by
  filename** because filesystem walk order would otherwise reshuffle the prompt between runs and
  cold-write the cache for nothing.
- Corrected a module doc that had claimed `context` assembles "per-turn" context — wrong since the
  `flux-context` crate fold, and the exact misconception that produced the bad acceptance criteria.

## Notes
- Source: [../research/amp.md](../research/amp.md) — Amp's AGENTS.md `@`-mentions plus YAML
  frontmatter `globs` on the mentioned files ("granular guidance").
- Shipped surface: `ContextFragments` in `crates/flux-runtime/src/context.rs`, registered after
  `ProjectFiles` in the CLI projector. Globs go through `flux_policy::wildcard_match` — the same
  matcher policy path grants use, so a guidance glob and a policy glob cannot disagree on syntax.
  No new dependency: `flux-runtime` already depended on both `flux-policy` and `flux-markdown`.
- Public docs: `website/docs/agent/project-context.md` (new page, sidebar-registered) — it also
  documents the previously-undocumented project-context chain as a whole.
- Deliberately **not** done: per-turn re-scoping, and an on-demand/progressive-disclosure mode for
  large reference fragments (that shape already exists for skills via D-188 `skill.load`; a fragment
  that wants it should probably just be a skill).
