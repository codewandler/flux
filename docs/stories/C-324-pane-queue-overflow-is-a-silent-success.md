---
id: C-324
title: "A dropped pane command is a silent success, against the surface's own stated posture"
pillar: Agent
status: ready
priority: 11
areas: [flux-tui, flux-runtime]
note: "found by C-305's review — PaneQueue::emit drops the newest command past MAX_PENDING_COMMANDS while the op still returns ok, but surface.rs's own posture is that the sibling failure (no sink) is 'a clear op failure (never a silent success)'; the channel is send-only so there is no evidence handle to report through, which is why this needs a posture decision rather than a patch"
---

# A dropped pane command is a silent success

## Goal

Make a dropped pane command observable, and bring the overflow path in line with the posture the
surface seam already states for its sibling failure.

`PaneQueue::emit` (`crates/flux-tui/src/panes.rs:185-193`) drops the newest command once
`MAX_PENDING_COMMANDS` is reached, and the `pane.*` op still returns ok. The model is told its pane
opened. It did not. Meanwhile `surface.rs` states that the *other* way this can fail — no sink
installed — is "a clear op failure (never a silent success)". The two failures on the same seam
answer differently.

**Drop-newest is the right choice of the two** and this story should not reverse it lightly:
drop-oldest would evict the `Open` and leave `Update`s that `PaneStore` discards anyway, which is
strictly worse. What is missing is not a different eviction policy but any record that eviction
happened.

**Why this is a story and not a one-line fix.** `PaneQueue::emit` has no evidence handle and no way
to report back — the channel is send-only, which is precisely why the drop is silent. Making it
observable means choosing a posture, and each option carries its own test surface:

- a transcript notice the operator reads;
- a counter surfaced through `apply_pending_panes` and drawn in the frame;
- reversing to a genuine op failure — which contradicts drop-newest, since by then the command is
  already gone and the op has returned.

That is a UX decision with consequences for what the model sees, not a mechanical gap.

## Acceptance

- [ ] **Pick the posture and write down what you rejected.** The three options above are the known
      candidates; the choice governs everything else in this story.
- [ ] **Failing-first**: a test that overflows the queue and observes the drop is currently invisible
      — no evidence record, no counter, nothing in the frame.
- [ ] The chosen signal is reachable from where it matters. If the model is told the pane opened, the
      model is the one being misled; decide whether the signal goes to the operator, the model, or
      both, and justify it.
- [ ] Drop-newest is preserved, or the reversal is argued explicitly against the reason above.
- [ ] **Reachability is stated honestly in the story.** Overflow requires 1024 pending commands
      inside one 62 ms frame, so this is a real-but-remote failure; if the fix costs more than the
      failure, say so and park it rather than building something elaborate.
- [ ] Full gate green in both workspaces.

## Notes

- Found by [C-305](C-305-run-tui-installs-the-surface-sink.md)'s review as a non-blocking item, and
  deliberately not fixed by its rework — the implementor's reasoning (send-only channel, posture
  decision, own test surface) is why it is filed rather than patched, and that judgement was right.
- Related: C-305 pinned the two delivery-chain links either side of this queue, so the surrounding
  wiring is now observed by tests. This is the one remaining unobserved outcome on that path.
- Adjacent, also from C-305's rework and not folded in here:
  `crates/flux-cli/tests/pane_surface_wiring.rs` leaves scratch workspaces (`flux-c305-*`) behind in
  `TMPDIR` — only `tui_surface_wiring` in `app_cmd.rs` cleans up after itself. Given this repo has
  already lost time to leaked `/tmp` fixture directories exhausting inodes, a sweep of test-fixture
  cleanup across the suite may be worth its own story.
