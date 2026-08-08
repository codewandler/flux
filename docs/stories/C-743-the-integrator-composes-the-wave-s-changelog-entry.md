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

- [ ] `fleet integrate` composes the wave's `## [Unreleased]` entry from its accepted stories, so a
      cut is never empty because nobody wrote prose.
- [ ] It writes once, on the assembled candidate, where the inputs are complete — the same reason the
      embedded-doc mirrors regenerate there and not per story.
- [ ] A story whose change is not user-visible contributes nothing rather than a filler line, and
      says so, so the entry is not padded to prove the step ran.
- [ ] The entry is verified after writing, not assumed: an integration that reports composing an
      entry has one in the tree.
- [ ] v0.59.2's notes are backfilled from the commits it contains, and its GitHub release is edited.
      The tag and binaries are correct; only the prose is missing.
- [ ] Regression test: a wave with two accepted stories produces one entry naming both, and a wave
      whose stories are all internal produces none.
