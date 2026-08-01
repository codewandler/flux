---
id: D-211
title: Agent screenshare — publish a rendered flux surface into the call
pillar: Agent
status: blocked
priority: 4
epic: meeting-rooms
design: docs/designs/meeting-rooms.md
areas: [flux-channels, flux-tui]
note: "⚠ UNPROVEN — the 2026-07-30 spike failed here: headless Chrome has no display so getDisplayMedia returns NotReadableError, and the Xvfb fallback never loaded the page. A canvas-captureStream track via lib-jitsi-meet (not the iframe API) is the path that avoids desktop capture entirely"
---

# Agent screenshare — publish a rendered flux surface into the call

## Goal

Let the agent **show** rather than tell: render a flux surface — a pane, a diff, a board view — and publish
it into the room as a video track, so humans in the call see what the agent is working on.

## Acceptance

- [ ] The agent publishes a video track sourced from a **rendered surface**, not a desktop capture:
      canvas → `captureStream()` → a track added via `lib-jitsi-meet` (the iframe/external API cannot
      inject a track, which is why this story does not use it).
- [ ] The surface content is an existing agent-authored pane (C-219…C-225), not a second, parallel
      rendering surface invented here.
- [ ] **Redaction on the render path.** Failing-first test `screenshare_publishes_redacted`: a pane whose
      source text contains a credential shape publishes redacted. Reuse the C-216 corpus — a screenshare
      publishes to every participant at once, so this is the highest-consequence render path flux has.
- [ ] Publishing video is an approved act (D-213), distinct from publishing audio.
- [ ] Frame rate and resolution are bounded and configurable; a static pane must not burn a core.

## Progress

- **Blocked on D-208 — screenshare rides the same media sidecar as D-210.** Recorded rather than left in `backlog`, so the board says *why* it is not takeable instead of implying nobody has decided.
- 2026-07-30 — **attempted and failed; recorded so the next attempt starts here.**
  1. `getDisplayMedia({video:true})` in headless Chrome 150 → `NotReadableError: Could not start video
     source`. Headless has no display to capture, and `--auto-select-desktop-capture-source` /
     `--auto-select-tab-capture-source-by-title` / `--auto-accept-this-tab-capture` did not change that.
  2. `api.executeCommand('toggleShareScreen')` produced no sharing participant
     (`contentSharingParticipants: []`).
  3. Fallback: real (non-headless) Chrome on an `Xvfb :99` display — Chrome started and held the URL, but
     the page never executed (no console beacon, no join) within 180 s. Unresolved.
  Conclusion: **stop chasing desktop capture.** Drive `lib-jitsi-meet` directly and add a canvas-sourced
  track, which needs no display and no capture permission at all.

## Notes
- This also removes the mirror problem: capturing the whole page would include the meeting iframe.
- Driving lib-jitsi-meet directly is a bigger sidecar change than the iframe API; sequence this after
  D-208 rather than in parallel.
