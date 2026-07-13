---
id: A-72
title: Surface staged intent progress in CLI and TUI
pillar: Agent
status: done
design: docs/designs/staged-intent-native-planning.md
note: "Make the staged router's existing intent/explore observations visible while each consultation runs, retain the accepted intent in scrollback, and include the silent prefix in CLI turn timing."
---

# Surface staged intent progress in CLI and TUI

## Goal

Remove the silent prefix from opt-in staged turns without changing their provider requests or
execution semantics. Text CLI and TUI users should see which phase is running, then retain a concise,
auditable summary of the accepted intent and capability narrowing.

## Acceptance

- [x] Failing-first: every staged provider consultation is bracketed by balanced
      `AgentSink::planning(true/false)` events; cancellation/error paths stop the indicator and a
      gather tool call never overlaps the consultation indicator.
- [x] `loop.phase = intent|explore` renders as `routing intent…` / `exploring…` in both CLI and TUI;
      non-TTY staged output emits a stable plain progress line instead of going silent.
- [x] The existing `turn.intent {intent,families,operations}` observation renders live and on TUI
      replay as a bounded `◆ intent` summary plus capability families and operation count. Exact
      operation names appear only in verbose mode.
- [x] CLI turn timing starts on the first planning consultation and is not reset by later rounds, so
      the final duration includes staged routing/exploration before the first dispatched operation.
- [x] The default planner's request and output remain unchanged; staged prompts, schemas, approvals,
      and execution are unchanged. Mock PTY/non-TTY checks and the existing staged protocol/E2E
      regression suite pass.
- [x] Workspace build/test/clippy/fmt/codegate, generated-doc sync, and the offline self-improvement
      flow smoke are green; changelogs and self-improvement status record the result.

## Progress

- 2026-07-13 — inspection found that A-71 already emits `loop.phase` and `turn.intent`, but direct
  `stream_blocks` calls bypass the existing planning lifecycle. As a result, the surfaces know the
  phase but never start their indicator, ignore the accepted intent, and the text CLI starts timing
  only at the first dispatched tool. This follow-up is rendering/lifecycle only.
- 2026-07-13 — failing-first tests reproduced all three gaps. Staged consultations now use the
  existing drop-balanced planning lifecycle; CLI/TUI render both phases and the accepted intent;
  redirected mock output shows the stable phase lines; a hanging-provider PTY probe showed the live
  `routing intent…` spinner and elapsed clock. Focused lifecycle, cancellation/error, CLI, TUI,
  replay, timing, and real-binary mock tests pass.
- 2026-07-13 — a fresh low-effort Gemini 3.5 Flash live regression passed the adversarial datasource
  fixture in session `s_1097`: four provider calls, six native calls, the narrowed
  `workspace.read` family, all required citations, and no fabricated path. Its visible CLI trace
  showed routing, accepted intent, and each exploration wait; the 12.5s turn footer now includes the
  formerly silent prefix (13.0s wall clock including process startup).
- 2026-07-13 — the final repository gate passed: workspace build and tests, strict all-target
  clippy, formatting, architecture layering, generated website sync, shell syntax for the staged
  regression runner, and the checked-in self-improvement smoke through all four guarded mock steps.

## Notes

- Normative staged protocol: [staged-intent-native-planning.md](../designs/staged-intent-native-planning.md).
- Do not add a second progress event or expose private reasoning; reuse the existing redacted
  observations and `AgentSink` planning lifecycle.
