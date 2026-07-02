---
id: A-19
title: Context-block injection (add_context) — <knowledge-base> in the system prompt
pillar: Agent
status: done
epic: grounded-knowledge
design: docs/designs/grounded-knowledge.md
note: the greenfield prompt-injection seam — small KBs grounded inline, no tool round-trip
---

# Context-block injection (add_context) — <knowledge-base> in the system prompt

## Goal
Give an agent a way to carry knowledge **in its system prompt** (not only via a retrieval tool): an
`AgentSpec.context` of blocks rendered as `<knowledge-base id=… title=…>…</knowledge-base>` after the
persona, byte-budgeted. This is the reusable primitive the ai-agents grounded-knowledge feature (small-KB
inject path, bare-agent grounding) builds on.

## Acceptance
- [ ] `AgentSpec` carries `context: Vec<ContextBlock{id,title,meta,body}>`; assembling the system prompt
      renders each block after `system_prompt` in order, wrapped in `<knowledge-base id="…" title="…">`…
      `</knowledge-base>`. **Failing-first test** in `flux-agent` (or wherever the prompt is assembled):
      two blocks render in order; empty `context` leaves the prompt byte-identical to today.
- [ ] Total rendered context is bounded by a byte budget; over-budget content truncates with a visible
      marker (never a silent drop) — asserted by test.
- [ ] SDK builder `FlowClient::builder().add_context(id, title, body)` + `AgentSpec::with_context(...)`.
- [ ] App path wired: `flux-app` `agent_spec_from_decl` renders injected blocks alongside the existing
      persona assembly (no change when no context is supplied).
- [ ] `render_knowledge_blocks(records, budget)` helper in `flux-capabilities::datasource` produces the
      same block text (so a consumer can inject datasource records) — unit-tested.

## Progress
- **Done.** `flux_core::ContextBlock` + `render_knowledge_blocks(blocks, budget)` render
  `<knowledge-base id=… title=…>` sections after the persona, budgeted with a visible truncation marker
  (string `meta` entries become extra attributes). `AgentSpec.context` + `context_budget` +
  `with_context(...)` + `effective_system_prompt()` compose them into the prompt (empty context ⇒
  byte-identical prefix, cache-stable). SDK `ClientBuilder::add_context(...)`; app `agent_spec_from_decl`
  reads `settings.context` (malformed ⇒ clean error); `flux_capabilities::records_to_context_blocks`
  bridges datasource records → blocks. Tests green across flux-core (5), flux-agent (2), flux-capabilities
  (1), flux-app (1); clippy clean.

## Notes
- Assembly point: `flux-agent/src/lib.rs` (`AgentSpec`, `DEFAULT_SYSTEM_PROMPT`), `spec.assemble(...)`;
  app persona at `flux-app/src/app.rs:791` (`agent_spec_from_decl`).
- Keep cache-stable: blocks ride in a system segment consistent with A-03's cache-first layout; static
  context before per-turn symbols.
- Design: [grounded-knowledge.md](../designs/grounded-knowledge.md). Consumer: ai-agents R-07 (inject vs.
  search) + A-09 (bare-agent persona).
