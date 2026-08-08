---
id: C-743
title: "The integrator composes the wave's changelog entry"
pillar: "Core"
status: backlog
priority: 1
epic: delivery-is-verified
areas: [flux-orchestrate]
---

# The integrator composes the wave's changelog entry

## Goal

GitHub release notes for **v0.59.2 are empty**. `## [0.59.2]` in `CHANGELOG.md` is three lines,
because `cut-release.sh` rolls `## [Unreleased]` into the version section and `[Unreleased]` was
empty across 5977 insertions — four autonomy links, six fleet-written stories, the receipt ledger,
the ops explorer and the crates.io guard.

The cause is structural. `.flux/fleet.toml` fences story workers out of `CHANGELOG.md` — correctly,
since `wave-346` became unintegrable when two stories each appended an entry — with the comment
"Assembling a wave-level changelog is the integrator's job". **Nothing in the integrator does it.**
The same shape as the integrator role itself, which was configured and never dispatched.

## Acceptance

- [x] `fleet integrate` composes the wave's `## [Unreleased]` entry from its accepted stories, so a
      cut is never empty because nobody wrote prose.
- [x] It writes once, on the assembled candidate, where the inputs are complete — the same reason the
      embedded-doc mirrors regenerate there and not per story.
- [x] A story whose change is not user-visible contributes nothing rather than a filler line, and
      says so, so the entry is not padded to prove the step ran.
- [x] The entry is verified after writing, not assumed: an integration that reports composing an
      entry has one in the tree.
- [x] v0.59.2's notes are backfilled from the commits it contains, and its GitHub release is edited.
      The tag and binaries are correct; only the prose is missing.
- [x] Regression test: a wave with two accepted stories produces one entry naming both, and a wave
      whose stories are all internal produces none.

## Progress

- `compose_unreleased_entry` in `crates/flux-cli/src/board_fleet_cmd.rs` builds one entry per wave
  per repository, after every cherry-pick and before the preparation step — the same point the
  derived mirrors are regenerated, because that is where the inputs are complete. It reuses
  `add_unreleased_changelog_entry` rather than adding a second changelog writer.
- Prose comes from each story's own **Goal**, read out of the candidate tree. The handoff summary was
  the other candidate and is not usable: a handoff recorded from a worktree carries the fixed string
  "recorded from the worker's worktree when its turn ended" for every story.
- User-visibility is decided from the handoff's observed write set. `PLANNING_ONLY_PATHS` names the
  paths that are how work is *tracked* rather than what the work *is* (`docs/stories/`,
  `docs/designs/`, `docs/decisions/`, the roadmap, the vision, `.flux/`); a story that wrote only
  those contributes nothing and its reason names the paths. Everything else ships or gates something
  that ships.
- `commit_unreleased_entry` re-reads the entry out of `git show HEAD:CHANGELOG.md` after committing.
  An entry that cannot be got into the tree is a failed integration for that repository, not a
  quieter success — the alternative is a candidate reporting prose it does not carry, which is the
  defect class this story exists to close.
- Backfilled `## [0.59.2]` from `git log v0.59.1..v0.59.2`. **The Goal's inventory of that release is
  wrong**: the four autonomy links (C-730, C-587, C-681, C-732) and the wave-761 stories (C-600,
  C-601, C-622) are *not* in v0.59.2 — they landed after the tag. C-519, C-544 and C-570 appear in
  the range only as mentions inside C-723's and C-724's commit bodies. What v0.59.2 actually contains
  is C-716, C-643 and C-575 as features; C-721, C-722, C-723, C-724, C-729/C-709 and the ssh
  transport-flake fix as fixes.
- `WHATS-NEW.md` gains a `[0.59.2]` section, and C-643's customer note moved there from `[0.59.1]`:
  the merge-forward filed it under a release that does not contain the feature. No shipped v0.59.1
  binary carries that claim — `git show v0.59.1:WHATS-NEW.md` has no ops entry — so the move corrects
  the website and future binaries without rewriting what any release said.
