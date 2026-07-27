---
id: C-91
title: Make approval prompts visible in the plain CLI
pillar: Core
status: done
---

# Make approval prompts visible in the plain CLI

## Goal
Without `--yes`, the plain (non-TUI) CLI's approval prompt is printed but immediately erased by the
stderr spinner (`\r\x1b[K` repaint every 80 ms, plus the deferred `stop_spinner` clear drained during
the approval wait) — the user sees a hung spinner yet `y` silently approves. Make the prompt own the
stderr line while it is open, and show *what* is being approved (ops, resources, commands) in the
whole-plan prompt.

## Acceptance
- [x] Unit: `prompt_gate_blocks_painting_while_held` — a held `PromptGate` yields no paint permits; released, it paints again.
- [x] Unit: `prompt_gate_suppresses_clear_only_while_held` — `painter_stopped()` is false while a prompt holds the gate.
- [x] Unit: `plan_prompt_lists_ops_subjects_and_answer_line` — the plan prompt renders ops, resource subjects, process commands, the destructive warning, and the `[y]es / [a]lways / [N]o: ` answer line.
- [x] Integration: `plan_approval_prompt_is_visible_with_piped_stdin` — `flux run -m mock` without `--yes`, stdin `y\n`: stderr shows the prompt + batch content and the write lands; with `n\n` it does not (`plan_approval_denied_on_n`).
- [x] Non-tty stdin path closes the prompt line (trailing newline) after reading the answer.

## Progress
- 2026-07-27: root-caused (spinner ticker overwrite + deferred `stop_spinner` clear via the race_turn
  event drain; batch observations unrendered in `CliSink`). Plan approved; implementing.
- 2026-07-27: shipped. `PromptGate` in rendering.rs (ticker `begin_paint` permits, suppressed
  `stop_spinner` clear), `read_choice` acquires it, `plan_prompt` carries the batch content,
  piped-stdin path closes the prompt line. Verified end-to-end under a pty (`script`): the full
  multi-line prompt survives with the spinner active; `y` approves; deny path leaves no file.
  Was C-89 at creation; renumbered to C-91 (another session claimed C-89/C-90 concurrently).

## Notes
- Fix lives entirely in `flux-cli`: `PromptGate` in rendering.rs (process-global — stderr is a
  process-global resource; no shared construction scope exists between `StdinApprover` and the
  per-turn `CliSink`s), acquire in `read_choice` (covers per-op, plan, and plugin-install prompts),
  enriched `plan_prompt` built from `PlanApprovalRequest` (requirements + Process intents).
- Deliberately NOT rendering `action_batch.proposed`/`approval.requested` in `CliSink::observation`:
  they drain after the prompt line is already open (ordering is structural to the mpsc sink drain).
  Routing CLI approvals through the sink channel TUI-style is a possible follow-up.
- tty spinner-wipe itself is not reproducible without a pty dev-dependency; every repaint/clear path
  flows through the unit-tested gate instead.
