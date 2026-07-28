---
id: C-132
title: Shareable run export — flux export <run> → one self-contained HTML file
pillar: Core
status: backlog
priority:
epic:
design:
note: "render a session/run (plan visuals via flow_render/render_styled, op results redacted per the evidence rules, diffs, cost, timeline) into a single static HTML file for bug reports, PR links, and demos — the read-only sibling of the Time Machine; no server, no viewer app"
---

# Shareable run export — flux export <run> → one self-contained HTML file

## Goal
Turn a recorded run into an artifact you can attach to an issue or PR: `flux export <run>` renders
the plan (via the `flow_render` substrate), redacted op results, diffs, cost, and a timeline into
one self-contained static HTML file. The read-only sibling of the Time Machine verbs.

## Acceptance
- [ ] `flux export <run> -o run.html` produces a single self-contained file (inline assets, no
  network) that renders the plan tree, per-op results, diffs, cost, and timeline — golden test
  over a recorded mock run.
- [ ] All content passes the durable-evidence redaction rules (C-22); a run containing a seeded
  secret exports with the secret redacted — failing-first test.
- [ ] Sub-agent children included (correlation per A-59) with clear nesting.
- [ ] Plan visuals reuse `flux_lang::highlight` / `render_styled` spans — no second renderer.
- [ ] Export is a pure read: no event-store writes, no provider construction.

## Progress
- (not started — filed from the 2026-07-28 feature-suggestion pass)

## Notes
- Deliberately not a web UI or server: a file. Complements the optional TUI cockpit (A-47).
