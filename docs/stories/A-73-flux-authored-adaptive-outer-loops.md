---
id: A-73
title: Make Flux-authored adaptive outer loops the agent runtime
pillar: Agent
status: done
design: docs/designs/adaptive-outer-loops.md
note: "Replace model-emitted Flux programs with typed stages, native operation calls, explicit batch approval, and one channel-neutral Flux-Lang outer loop."
---

# Make Flux-authored adaptive outer loops the agent runtime

## Goal

Make the successful A-71 intent/evidence/capability protocol the default agent architecture without
retaining its hybrid fallback. The outer loop is an authored Flux-Lang program; model-backed stages
produce their own typed values and provider-native operation calls; the host freezes proposed effects
into an ordered action batch, obtains explicit approval, and executes through the existing envelope.
The model never authors executable Flux.

## Acceptance

- [x] Failing-first: operation output schemas survive registration and become the analyzer's inferred
      result type; SDK-defined stages may use unrelated typed input/output contracts.
- [x] Failing-first: the built-in adaptive loop is the default on every conversational text agent surface and is an
      ordinary validated Flux-Lang flow composed from intent, exploration, batch, approval, execution,
      question/await, and presentation operations. Explicit flow-driven and model-driven realtime
      modes retain their documented outer-loop ownership.
- [x] Intent and later-stage signals only surface registered, wired operations within the agent's
      tool/permission/capability ceiling. Tool staging is explicit and cannot label a mutating or
      destructive operation gather-safe.
- [x] Effectful native calls become a host-built `ActionBatch`. `approve_batch` yields an opaque,
      batch/session/caller-bound receipt; `execute_batch` rejects missing, stale, changed, reused, or
      denied receipts and dispatches every action through `Executor`.
- [x] Execution errors return to the same native-stage ledger for a local correction and final
      presentation. No legacy `plan` call or whole Flux program regeneration occurs.
- [x] A stage may return a typed decision request. The outer loop presents it, parks on the existing
      `await`, and resumes the same state on CLI, SDK/A2A, and voice adapters.
- [x] `AgentSpec`, SDK builders, app agent declarations, roles, CLI flags, and config can select a
      built-in or explicit Flux loop. `.flux/agent-loop.flux` no longer overrides behavior implicitly.
- [x] Config-defined model stages and SDK `stage_fn<I, O>` stages register as ordinary guarded typed
      operations; no common stage-result envelope is required.
- [x] Remove the NL-to-Flux compiler, `emit_plan`/`plan`, `PlanningMode`/`--staged`, `flux plan`, and
      NL `FlowClient::compile`. Keep authored Flux parsing/analysis/execution and historical event reads.
- [x] The Northwind live matrix passes 3/3 on Codex, Gemini Flash, DeepSeek V4 Flash Nitro, and
      GPT-5-mini where available, with zero legacy planner calls and no provider-call regression.
- [x] Full workspace, clippy, formatting, codegate, generated-doc, SDK, CLI, A2A, and hermetic voice
      gates pass; changelogs and self-improvement status report measured results honestly.

## Notes

- The normative design is [adaptive-outer-loops.md](../designs/adaptive-outer-loops.md).
- `ai-agent-platform` adopts only after the Flux breaking release. This story does not modify that
  repository.

## Verification

- The installed `flux` binary passed `scripts/eval-adaptive-support.sh` 12/12 on 2026-07-13: Codex
  gpt-5.5 (`s_1150`–`s_1152`), Gemini 3.5 Flash (`s_1153`–`s_1155`), DeepSeek V4 Flash Nitro
  (`s_1156`–`s_1158`), and GPT-5-mini (`s_1159`–`s_1161`). Every trial cited the required real
  files, fabricated no path, and made zero legacy planner calls. Provider calls stayed within the
  fixed per-model budgets: 4/4, 4/4, 4–5/7, and 4/6 respectively.
- Missing or wildcarded paths now return deterministic inventory guidance, and the exploration stage
  inventories the workspace before reading when the user did not supply an exact path. This closed
  the only initial matrix miss without relaxing the gate.
- `task install` completed before and after the final path-discovery hardening, including all 110
  `flux-system` tests. The workspace build/test/clippy/fmt/codegate gate and generated documentation
  sync are green.
