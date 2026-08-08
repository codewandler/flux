---
id: C-633
title: "A worker session store survives its turn for post-mortem"
pillar: "Core"
status: backlog
epic: session-truth
areas: [flux-cli]
note: "session_mode: fresh discards the store; a failed worker leaves only the unordered activity projection; the retro pipeline (roadmap R-22) needs the transcript. Complements C-602 which streams a live bounded projection"
---

# A worker session store survives its turn for post-mortem

## Goal

A `session_mode: fresh` worker's session store is discarded with its process, so a failed worker
— the highest-signal input for any retrospective — leaves only the unordered activity projection.
The roadmap retro pipeline (R-22) reads worker conversations after the fact; it needs the durable
transcript, not a live stream. C-602 keeps worker stores isolated and streams a bounded live
projection; this story makes the isolated store itself outlive the turn.

## Acceptance

- [ ] A fleet worker's session store persists after its process exits, success or failure, under the wave's durable state, with a retention bound.
- [ ] `flux fleet inspect` (or session query, C-590) can open a dead worker's transcript by agent id.
- [ ] Reclamation of wave storage does not delete the transcripts of turns that failed.
