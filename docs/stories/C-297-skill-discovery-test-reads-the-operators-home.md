---
id: C-297
title: "A skill-discovery test reads the operator's real `~/.claude/skills`, so a concurrent session reds the gate"
pillar: Core
status: ready
priority: 6
areas: [flux-runtime]
note: "found by C-213 and hit by the coordinator on the very next merge — `discover_skills` walks the machine's real home, so any agent session writing there fails an unrelated test and the log looks like a code regression"
---

# A skill-discovery test reads the operator's real `~/.claude/skills`

## Goal

`crates/flux-runtime/src/metadata.rs`'s
`skill_directory_with_no_frontmatter_name_takes_directory_name` calls `discover_skills`, which reads
the machine's **real** `~/.flux/skills`, `~/.agents/skills` and `~/.claude/skills` rather than a
pinned fixture root. On a machine where those directories are live — a developer box, and especially
one running several agent sessions — the test's result depends on what another process happened to
write a second earlier.

It is a gate flake with a bad failure signature: the run aborts, the log truncates, and it reads as a
compile or infrastructure failure in whatever diff is being merged.

## Acceptance

- [ ] The test resolves its skill roots from an injected/overridden home, not from the process's real
      one. `HarnessEnv` in `flux-capabilities` (C-213) is the shape that already exists in this tree
      for exactly this problem — an injected environment rather than per-site `std::env` reads — and
      is worth copying rather than reinventing.
- [ ] A failing-first demonstration: with a stray directory planted in the real `~/.claude/skills`,
      the test fails at the merge base and passes after. That is reproducible without waiting for a
      concurrent session to collide.
- [ ] Any *other* test in the workspace that reads a real home directory is enumerated in this story.
      Fixing one instance of a class and leaving its siblings is how this recurs. `discover_skills`
      is the known one; say plainly whether the scan found others and how you scanned.
- [ ] Full gate green, twice in a row, while something concurrently writes to `~/.claude/skills`.

## Notes

- **Provenance, and why it is filed rather than shrugged off.** C-213's implementor hit it once in
  ten full-workspace runs, investigated properly rather than re-running until green, and proved it
  pre-existing: the test lives in a crate its diff never touched, and
  `cargo tree -p codewandler-flux-runtime -e normal,dev` contains neither `flux-capabilities` nor
  `flux-cli`, so its code was not in that binary's closure at all. It then failed to reproduce in six
  runs at the merge base.
- **The coordinator then hit it immediately**, on the very next merge, while a review agent was
  running concurrently: `cargo test --workspace` exited 101 having run 46 tests, with a truncated log
  and no error line. A clean re-run gave 3392 passed / 0 failed. Two independent hits in one session
  is not a curiosity; it is a tax on every merge in a multi-session repo.
- ⚠ This is the same family as
  [`tmp-git-flaky-sticky-test`](../../CLAUDE.md) — a test whose result depends on machine state
  outside the repository. The general rule worth stating in the fix: a unit test may not read the
  operator's home.
- Related: [C-213](C-213-extract-harness-adapters.md) found it and named the fix shape.
