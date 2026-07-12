---
id: A-68
title: Wire reasoning effort through the complete agent call graph
pillar: Agent
status: backlog
note: "2026-07-13 live latency audit: --effort is parsed but explicitly discarded after the engine cutover, so low/high experiments are currently placebo flags."
---

# Wire reasoning effort through the complete agent call graph

## Goal

Make `--effort low|medium|high|xhigh|max` a real, documented agent setting rather than a hidden
compatibility no-op. One selection must consistently reach every model call made for the turn:
planner, completion renderer, context compaction, cognition operations, and inherited sub-agents.

## Acceptance

- [ ] `AgentSpec` owns an optional typed effort setting and passes it into `FlowEngine` without
      breaking existing constructors/builders.
- [ ] Planner/repair calls, the completion fast path, compaction, and `CognitionPack` requests carry
      the same effort; sub-agents inherit it unless their role explicitly overrides it.
- [ ] Provider codecs continue applying their existing capability gates/mappings. Unsupported models
      omit or reject the setting honestly; Flux does not silently claim it was applied.
- [ ] `--effort` is visible in CLI help and provider docs no longer call it a no-op. Decide and
      document whether legacy `--think` is an alias or removed.
- [ ] Capture-provider tests assert the request setting on every call class above.
- [ ] Live OpenRouter or Codex low-vs-high probes record request usage, reasoning tokens, latency,
      and correctness on the same task. The result is reported without treating noisy single runs as
      a universal default recommendation.

