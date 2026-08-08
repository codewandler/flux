---
id: C-748
title: "A story the board cannot parse is a failure, not a silent skip"
pillar: "Core"
status: ready
areas: [flux-cli]
depends_on: [C-736]
design: docs/designs/story-contracts-are-validated.md
note: "C-320 sat at `status: active` — not a parseable value — and was dropped by every board read while `check` exited 0. Invisible is worse than invalid."
---

# A story the board cannot parse is a failure, not a silent skip

## Goal

A `.md` file in `docs/stories/` that the board cannot parse — bad frontmatter, or a `status:` that is
not a legal value — is currently **dropped** and reported as a warning, and `flux board check` exits
`0`. So the story exists on disk, is invisible to `list`, `stats`, `reconcile`, dispatch and every
other board read, and nothing ever fails.

`C-320` sat at `status: active` in exactly this state. Its work had shipped, its last criterion was
satisfiable from `main`, and no board surface could see any of it. It was found by reading the
warning line by hand, which is the only place it appeared.

Invisible is worse than invalid: an invalid story gets fixed, an invisible one accumulates. This
story makes an unreadable story a hard `check` failure, so the board's census is the whole of
`docs/stories/` or the board says why not.

## Acceptance

- [ ] **Failing-first**: a fixture board containing (a) a story whose `status:` is not a legal value
      and (b) a story whose YAML frontmatter does not parse. `flux board check` exits non-zero and
      names both files and the reason for each. The test asserts the exit status, not just the text.
- [ ] The message states the legal statuses, so the fix is readable from the failure without opening
      the source.
- [ ] `list`, `stats` and `reconcile` keep their current drop-and-warn behaviour — they are reads,
      and a read that hard-fails on one bad file makes the board unusable while it is being fixed.
      `check` is the surface whose entire job is to fail. Assert this split with a test so a later
      change cannot quietly promote the warning everywhere.
- [ ] A story that is dropped is still counted: `check`'s summary line reports the number of files it
      could not read alongside the number it validated, so `1783 stories` can never again silently
      mean `1784 files, one unreadable`.
- [ ] `flux board check` passes on the real repository after the change, with `C-320` fixed at its
      source (already done on `main` — it is `done`, and is the regression fixture).
- [ ] Full gate green: `scripts/release-full-gate.sh`.

## Notes

- Found while migrating the board for [C-736](C-736-board-check-validates-the-contract-a-story-is-dispatched-against.md).
  C-736 validates the *body* of a story the board can read; this closes the case where it cannot read
  it at all. The two are independent and C-736 does not cover this — its check never runs for a file
  that was dropped before it.
- `read_stories_with_warnings` in `crates/flux-cli/src/board_fleet_cmd.rs` is where the drop happens;
  the warning it pushes is the string that has to become an error at the `check` call site.
- Measured 2026-08-08: exactly one story in 1,783 was in this state, so the migration cost is one
  file. That is the argument for doing it now rather than after the count grows.
