---
id: A-71
title: Add an opt-in intent-routed native-schema planning loop
pillar: Agent
status: done
design: docs/designs/staged-intent-native-planning.md
note: "Cross-model PoC: make the adversarial support-workspace E2E reliable by routing intent first, gathering through real operation schemas, and approving a frozen Flux plan before mutations."
---

# Add an opt-in intent-routed native-schema planning loop

## Goal

Prove that Flux can preserve its deterministic runtime while letting several model families use the
operation schemas they already understand well: classify intent into a narrow capability signal,
gather through provider-native tool calls, capture rather than execute mutations, lower the captured
atomic calls into Flux-Lang, approve the frozen plan, then execute it through the existing envelope.

The PoC is Flux-only and opt-in. The shipped planner remains the default, and no platform repository
or downstream API is changed.

## Acceptance

- [x] Failing-first: default agents take the existing `plan` path without an extra provider call or
      a changed request body; only an explicit `AgentSpec`/CLI opt-in enters the staged path.
- [x] Failing-first: the first staged provider call can invoke only a typed `declare_intent` tool.
      Its capability-family enum is derived from operations that are actually registered; selecting
      a family narrows visibility but never grants permission.
- [x] Failing-first: the exploration call receives the selected operations as provider-native
      `ToolDef`s with their exact `ToolSpec.input_schema`; unselected or fabricated calls get an
      actionable tool result and never dispatch.
- [x] Failing-first: gather-safe calls execute as one-call Flux microplans through the existing
      `Executor`; mutating/non-idempotent calls are returned to the model as captured plan steps and
      have zero effects before finalization/approval.
- [x] Failing-first: `finalize_plan` lowers captured literal calls into a sequential `DraftAst`, runs
      the normal analyzer/lowering gate, surfaces the ordinary `flow.plan`, and delegates approval
      plus execution to the unchanged `run_plan` path. Denial executes zero captured calls.
- [x] Failing-first: malformed/missing intent declarations, unknown capability families, malformed
      native calls, mixed finalization calls, and round exhaustion terminate or repair honestly;
      cancellation and usage accounting cover every provider call.
- [x] A repeatable `/tmp` E2E fixture recreates the Northwind/ORB-17 support task and grades all
      three facts plus the four exact source paths. At least three fresh trials each pass on Codex,
      OpenRouter Gemini Flash, OpenRouter DeepSeek V4 Flash Nitro, and the earlier GPT-5-mini control
      when that route is available. Latency, provider calls, selected families, native calls, and
      fabricated paths are recorded per trial.
- [x] Full workspace build/test/clippy/fmt/codegate and the offline self-improvement flow smoke are
      green; engineering/customer changelogs and self-improvement status record the honest result.

## Progress

- 2026-07-13 — branch `poc/staged-intent-loop` created from the current hardened Flux head. The
  earlier raw sessions and `/tmp/flux-adhoc-e2e-20260713` fixture were recovered: Codex was correct
  but sometimes cited invented filenames; Gemini fabricated all central facts once in three trials;
  DeepSeek was correct but used 4–10 model calls. Design written before implementation.
- 2026-07-13 — implemented the opt-in single-loop branch, native intent/exploration protocol,
  complete JSON-Schema validation, gather microplans, inert action capture, ordinary plan approval,
  usage/audit observations, CLI flag, and repeatable `/tmp` live matrix runner. Offline tests caught
  and fixed a non-total Flux `match` and a shared `PlanRisk` gap where declared write effects without
  duplicated intent tags skipped aggregate approval.
- 2026-07-13 — live trials exposed two provider seams and drove deterministic fixes: a captured
  non-idempotent `now()` action lost earlier native gather evidence at completion, so completion now
  carries a bounded host-derived evidence primer; Codex rejected dotted cognition operation names,
  so native definitions now use stable reversible aliases while execution keeps canonical Flux
  names. Passing trials were observed on Codex gpt-5.5, Gemini 3.5 Flash, DeepSeek V4 Flash Nitro,
  and GPT-5-mini; those findings were folded in before the final post-fix matrix below.
- 2026-07-13 — final-build low-effort matrix passed 3/3 on every target: Codex gpt-5.5 sessions
  `s_1078`–`s_1080` (17.4–25.2s, 4 provider calls), Gemini 3.5 Flash `s_1087`–`s_1089`
  (13.2–15.9s, 4 calls), DeepSeek V4 Flash Nitro `s_1084`–`s_1086` (21.4–26.8s, 4–7 calls), and
  GPT-5-mini `s_1075`–`s_1077` (16.4–20.3s, 4–6 calls). Every persisted answer contained the three
  exact conclusions and all four real source paths; answer-only path extraction found no fabricated
  `.md`, `.csv`, or `.json` citation.
- 2026-07-13 — the complete repository gate passed: workspace build and tests, strict all-target
  clippy, formatting, architecture layering, generated website changelog sync, and the checked-in
  `eval-smoke.flux` flow through the mock adapter. The first non-interactive smoke correctly stopped
  at approval; the explicit `--yes` test run then completed all four guarded steps.

## Notes

- The normative design is [staged-intent-native-planning.md](../designs/staged-intent-native-planning.md).
- This story does not replace `emit_plan`, remove Flux-Lang, weaken the gather effect gate, or modify
  `ai-agent-platform`.
