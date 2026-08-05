---
id: C-523
title: "Add deterministic usage replay and bounded animation"
pillar: Core
status: backlog
epic: usage-observatory
note: "Drive the static observatory with a virtual clock, seekable playback, coalesced pulses and bounded per-frame work"
---

# Add deterministic usage replay and bounded animation

## Goal

Add a deterministic virtual replay clock and explanatory route animation to C-522 without changing
its exact analysis. Playback remains seekable and responsive from 4-hour windows through dense
seven-day histories.

## Acceptance

- [ ] A failing-first test named `virtual_clock_replay_is_frame_deterministic` proves the same fixture,
      range, clock advances, speed, and viewport produce identical cursor positions, visible pulses,
      frames, and cumulative totals.
- [ ] Playback supports play/pause, restart, forward and backward seek, speeds from 0.5× through 100×,
      and fit-to-duration. Range or filter changes reset/rebase playback by a documented deterministic
      rule rather than retaining an invalid cursor.
- [ ] One usage-bearing call produces one pulse when call-level time exists. Coarser timestamp sources
      remain labelled, and route-identical dense calls coalesce into bounded `×N` pulses without
      changing exact totals or cost coverage.
- [ ] A failing-first test named `seek_backward_restores_checkpointed_totals` proves backward seek and
      restart reproduce the same cumulative state as replaying from the beginning, using checkpoints
      rather than rescanning full history on every frame.
- [ ] A seven-day stress fixture in `dense_replay_keeps_frame_work_bounded` proves memory, visible pulse
      count, and per-frame rendering work remain bounded and that input processing is not starved by
      animation.
- [ ] Pausing or disabling animation leaves every C-522 control, exact total, comparison, grouping, and
      inspector usable; reduced-motion presentation introduces no hidden metric or color-only meaning.
- [ ] Snapshot/state coverage adds pause, seek-backward, range-change, speed-change, fit, and coalesced-
      burst states to C-522's layout matrix.

## Progress

- (not started)

## Notes

- Depends on [C-522](C-522-static-usage-observatory-tui.md).
- Animation is a projection of C-521's buckets and C-520's usage facts, never an accounting source.
