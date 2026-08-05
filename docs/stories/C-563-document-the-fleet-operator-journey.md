---
id: C-563
title: "An operator can start, talk to and watch a Fleet from the public guide"
pillar: Core
status: ready
priority: 4
epic: fleet-loop
design: docs/designs/native-board-fleet-cli.md
areas: [docs, website]
depends_on: [C-561]
note: "public-doc gap — commands exist, but the first working main-agent conversation and watch loop are not taught as one journey"
---

# An operator can start, talk to and watch a Fleet from the public guide

## Goal

Give a new operator one copy-paste journey that makes the main agent respond, launches a bounded
wave, and shows real progress without requiring knowledge of Flux internals or tmux.

## Acceptance

- [ ] The public Fleet page opens with prerequisites and a ten-minute path: initialize or validate a
      workspace, start main, send one requirement to `main`, wait for a completed acknowledgement,
      inspect the resulting schedule, run explicit BoardRefs and watch progress.
- [ ] A command table distinguishes `ingest`, `message main`, `message WORKER`, `task`, `run` and
      `note`, including the exact meaning of `accepted`, `delivered` and `completed`. It says plainly
      that the default `ingest --wait accepted` journals only; it does not claim the model answered.
- [ ] The guide shows a complete three-repository, five-writer configuration and explains all three
      admission ceilings: global `max_workers`, template `max_instances` and per-repository
      `concurrency`.
- [ ] Watching is honest: `dashboard` is a point-in-time terminal view and `events --follow --output
      ndjson` is the live stream. The guide does not call either the future polished TUI from C-556/
      C-557 and does not present tmux panes as a control channel.
- [ ] Public navigation places Concepts before Coding, and the Coding guide uses readable text
      diagrams for the planning lifecycle, dispatch eligibility, worktree topology, writer/reviewer
      loop and the separate integration/apply/publication completion boundaries.
- [ ] Recovery examples cover failed worker inspection, acknowledged steering, resume, rework,
      integrate and explicit local-only apply, with push/release/deploy still separate.
- [ ] Public link/command fixtures and the website build pass; the legacy agent/fleet redirect and
      installed-version guide point at the same operator journey.

## Notes

- Filed directly from operator feedback on 2026-08-05.
