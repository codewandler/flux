---
id: A-67
title: Surface installed plugin operations only when the turn names the integration
pillar: Agent
status: done
design: docs/designs/turn-intent-plugin-surfacing.md
note: "Live tutorial E2E found 636 registered ops and a 27.5k-token plugin catalog tax on an unrelated two-file task."
---

# Surface installed plugin operations only when the turn names the integration

## Goal

Keep globally installed integrations available without injecting every ungrouped plugin operation
into every planner request. An unrelated turn should pay for Flux-Lang and core operations, not
hundreds of Slack/GitLab/Kubernetes/etc. schemas; a turn that names an integration should still see
that integration's operations and execute through the unchanged safety envelope.

## Acceptance

- [x] Failing-first: an ungrouped plugin operation belongs to an implicit per-plugin group instead
      of remaining an always-advertised core operation.
- [x] Failing-first: a whole-token integration mention activates the implicit group for that turn;
      unrelated text and substring collisions do not.
- [x] Explicit plugin-authored groups remain authoritative. `FLUX_SURFACE_ALL` still exposes the
      complete catalog as the operator escape hatch.
- [x] Activation is sticky for the engine session, preserving the monotonic prompt-cache contract.
- [x] `groups.active` records the inferred turn-intent signal so the catalog decision is auditable.
- [x] Live before/after with the same OpenRouter prompt and installed plugins materially reduces
      planner input from the 41,567-token baseline while preserving a named-plugin request.
- [x] Full workspace gate and the offline self-improvement flow smoke are green.
- [x] Engineering/customer changelogs and self-improvement status record the measured result.

## Live baseline (2026-07-13)

- `openrouter/openai/gpt-5-mini`, trivial prose turn, normal HOME: **41,567 input tokens**, $0.0106.
- Same binary/model/prompt/workspace, isolated HOME with no plugins: **~14,100 input tokens**, $0.0032.
- The normal installation registered **636 operations**; only `browser`, `cognition`, and `endpoint`
  groups were active. Most plugin operations had no group, so the runtime classified them as core.
- A real Codex tutorial turn completed correctly in 18.65s with `--yes`; the earlier 81.6s report
  folded approval response delay into op durations. That telemetry issue is a separate follow-up.

## Result (2026-07-13)

- Unrelated OpenRouter probe: **41,567 → ~14,100 input tokens**, $0.0106 → $0.0025.
- Slack-named probe: `plugin.slack` was the only plugin group added; **15.3k input tokens**.
- Exact interactive Codex tutorial task in a real PTY, with approval supplied immediately: complete
  file and all required facts in **21.8s wall**, one plan approval, **12,117 planner input tokens**.
- Workspace build/test/clippy/fmt/codegate, website build, and approved offline mock improvement smoke
  passed. The literal non-TTY smoke command denied `eval_run` at the approval envelope as designed;
  `--yes` executed the same local mock graph successfully.
