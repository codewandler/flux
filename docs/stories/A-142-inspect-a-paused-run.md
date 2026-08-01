---
id: A-142
title: "Inspect a paused run — read-only first, redacted, and it cannot corrupt anything"
pillar: Agent
status: backlog
design: docs/designs/interactive-debugger.md
epic: interactive-debugger
areas: [flux-tui, flux-secret]
note: "read-only deliberately: useful alone, cannot corrupt a run, and it is what makes the write half reviewable. ⚠ Inspection is a DISCLOSURE surface — a paused run holds tool outputs and possibly secret material, and in a demo the debugger pane is on a shared screen"
---

# See what the run is holding

## Goal

A paused run's state is readable in the TUI, redacted, with no effect on the run.

## Why read-only first

It is genuinely useful alone — most debugging is looking — it cannot corrupt a run, and it is the half
that makes [A-143](A-143-change-a-value-and-continue.md)'s mutation reviewable.

## Acceptance

- [ ] **Failing-first**: a test asserting a paused run's state is readable and that reading it does not
      advance or alter the run — failing at the merge base.
- [ ] ⚠ **Everything shown routes through the `Redactor`.** A paused run holds tool outputs and may hold
      secret material, and this pane is exactly what is on screen during a demo or a screenshare.
- [ ] ⚠ **The redaction failure path is tested, not just the happy one.**
      [C-339](C-339-redaction-falls-back-to-the-unredacted-value.md) found redaction in this codebase
      failing **open** — returning the unredacted value when redacted text stopped parsing. Assume the
      same class is reachable here.
- [ ] State is rendered in the same vocabulary the plan and thread views use, not a third one.
- [ ] ⚠ **Which state is addressable is decided and documented**: Flux-Lang locals, the context pack, the
      provider ledger and the tool registry are all "state" and are not equally safe to expose. An
      undecided scope becomes whatever the implementation happened to reach.
- [ ] Full gate green.

## Notes

- Depends on [A-140](A-140-pause-a-live-run.md) — there is nothing to inspect without a defined pause.
- ⚠ Much of this exists offline already: `flux replay` (A-45) and `flux export` inspect a recorded run,
  and `export_cmd.rs` renders `plan_source` through the redactor. Read that path before building a
  second one — the live case differs, the redaction discipline should not.

## Progress

- Filed 2026-08-01 with the interactive-debugger epic.
