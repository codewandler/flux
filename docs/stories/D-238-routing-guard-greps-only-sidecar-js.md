---
id: D-238
title: "The device-routing guard greps only `sidecar.js`, so `page.js` can reintroduce what it forbids"
pillar: Agent
status: ready
priority: 5
epic: meeting-rooms
design: docs/designs/meeting-rooms.md
areas: [flux-channels]
note: "the guard asserts the forbidden routing APIs are absent by grepping one file; page.js already contains all three names in comments, so setAudioInputDevice reintroduced there would pass"
---

# A guard that checks one of the two files it is about

## Goal

Make the device-routing guard cover every asset that could reintroduce the routing APIs it forbids, so
its green is evidence rather than a coincidence of which file it happened to read.

## The finding

The routing guard (`crates/flux-channels/tests/room_media_harness.rs:209`) asserts that the forbidden
device-routing APIs are absent — by grepping **`sidecar.js` only**.

`page.js:30-31` already contains all three forbidden flag names, in comments. So the guard's subject
matter demonstrably lives in both files, and a genuine reintroduction of `setAudioInputDevice` in
`page.js` would **pass** the guard.

⚠ This is the repo's recurring defect class in its most ordinary form: a guard whose scope is narrower
than the invariant it is named for, reading as if it covered the invariant. The invariant is "flux does
not route devices"; the guard is "one file does not mention routing".

## Acceptance

- [ ] A failing-first test: `setAudioInputDevice` (or either of the other two forbidden names) placed in
      `page.js` **fails** the guard. It must pass — i.e. go unnoticed — at the merge base.
- [ ] The guard covers every asset under `crates/flux-channels/assets/room-media/`, enumerated from the
      directory rather than by a hand-maintained file list — a list is what drifted here in the first
      place, and a fourth asset added later must be covered without anyone remembering to add it.
- [ ] ⚠ The comments at `page.js:30-31` that legitimately *name* the forbidden APIs must not force the
      guard to be weakened to accommodate them. Distinguish a mention from a call, or move the
      explanation somewhere the guard does not read — but do not solve it by exempting `page.js`, which
      reproduces the current bug under a different name.
- [ ] The guard's own message says what it checked, so a future reader can tell its scope without
      reading its implementation.

## Notes

- Surfaced by D-232's review, 2026-08-02. The guard is D-232's own, so this is a narrowing of new work
  rather than a pre-existing gap — it was reported as a follow-up rather than a rework because D-232's
  blocking finding was elsewhere and the guard is sound within the file it reads.
- The invariant itself is real and worth guarding: flux states which room to join and what to publish,
  and the sidecar owns *how* a capture device is chosen (`crates/flux-channels/src/config.rs:366-368`).
  A flux-side device-routing call would breach that split.
- Related: [D-232](D-232-the-media-sidecar-harness.md),
  [D-235](D-235-argv-alone-does-not-reach-the-audio-server.md).
