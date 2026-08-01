---
id: C-324
title: "A dropped pane command is a silent success, against the surface's own stated posture"
pillar: Agent
status: done
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

- [x] **Pick the posture and write down what you rejected.** The three options above are the known
      candidates; the choice governs everything else in this story.
- [x] **Failing-first**: a test that overflows the queue and observes the drop is currently invisible
      — no evidence record, no counter, nothing in the frame.
- [x] The chosen signal is reachable from where it matters. If the model is told the pane opened, the
      model is the one being misled; decide whether the signal goes to the operator, the model, or
      both, and justify it.
- [x] Drop-newest is preserved, or the reversal is argued explicitly against the reason above.
- [x] **Reachability is stated honestly in the story.** Overflow requires 1024 pending commands
      inside one 62 ms frame, so this is a real-but-remote failure; if the fix costs more than the
      failure, say so and park it rather than building something elaborate.
- [x] Full gate green in both workspaces.

## Progress

**Posture: the drop is counted and reported to the *operator*, in the transcript. Drop-newest is
kept unchanged, and the surface's stated promise is not weakened.** Which side moved: the
behaviour, not the promise. `flux-tools`' surface module keeps saying the sibling failure is "a
clear op failure (never a silent success)"; this story makes overflow stop being a silent success
on the one channel it actually has.

Options 1 and 2 were taken together and are the same mechanism: `PaneQueue` counts what it refuses
(the counter), and `ChatState::apply_pending_panes` turns a non-zero count into a transcript
`Notice` the operator reads and the frame draws. The counter alone would have been another
unobserved number; the notice alone would have had nothing to count.

**Option 3 — reversing to a genuine op failure — was rejected, and not for the reason the story
gives.** The story's reason ("by then the command is already gone and the op has returned") does not
hold under drop-newest: `SurfaceSink::emit` is called synchronously from inside the live `pane.*`
op, so the op *could* be told. The real reason is cost. Telling the op means `SurfaceSink::emit`
returning acceptance — a breaking change to a trait published in `codewandler-flux-runtime`, plus
`SurfaceReporter::send`, `PaneQueue`, three test sinks and all three `pane.*` op bodies, and a MINOR
bump under this repo's pre-1.0 SemVer rule. A wide, cross-layer, version-bearing change to close a
hole that needs 1024 pending commands inside one 62 ms frame. That is the fix costing more than the
failure, so it is parked here rather than built.

**So the model is not told, deliberately — and it has no compensating recourse.** It is the party
being misled, and this story does not close that; stated plainly rather than papered over. There is
**no read-back on this surface at all**: `pane.list` is not registered anywhere
(`PANE_OPS` in `crates/flux-tools/src/surface.rs` is the three write ops) and
`docs/designs/agent-authored-surface.md` says it does not ship. Settling that is exactly what
[C-306](C-306-pane-read-back-contract.md) exists for, and until C-306 lands there is nothing a model
can call to discover that a pane it "opened" is absent. What C-324 buys is the operator's half —
previously missing entirely: when a pane the agent claims to have opened is not there, the surface
now says why instead of leaving the operator to conclude it is broken.

⚠ An earlier draft of this section, and of the comment at the drop site, claimed `pane.list` gave
the model that recourse. It does not exist. The claim is recorded here because inventing a
compensating control is the same defect class this story exists to close, and C-306's Acceptance is
specifically about deleting comments that assert wiring which will never exist.

**Reachability, honestly.** Remote. The channel is drained at the top of every event-loop iteration,
so overflow needs 1024 pane commands emitted between two frames. No legitimate turn comes close; the
realistic causes are a looping tool and a bug. The fix is correspondingly small — a counter, one
edge-triggered notice, no new types on any public seam.

The notice is **edge-triggered**: reported when the channel starts refusing and again only after it
has recovered. A per-frame notice would bury the transcript under the symptom it describes. The
consequence is that the number in the notice is *that frame's* count, not a running total of a
sustained flood — so the text says "dropped in this frame, and more will be for as long as it stays
full" rather than a figure that would quietly understate by orders of magnitude.

Test: `panes::tests::a_dropped_pane_command_is_reported_to_the_operator`
(`crates/flux-tui/src/panes.rs`). It asserts on the transcript and the drawn frame, not on a return
value — the op's return is `ok` before and after this change, so a test that watched it would pass
at the base and prove nothing.

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
