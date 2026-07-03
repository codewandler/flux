---
id: A-22
title: "Compact served / agentic / SDK agents — bound the unbounded conversation"
pillar: Agent
status: done
epic: library-hardening
design: docs/designs/library-hardening.md
note: "the app agent-target is built with AgentSpec::default() → compact_threshold_chars = 0 → maybe_compact is a no-op; only the CLI ever sets it, so a persistent-session Slack/agentic target grows unbounded until the context window blows"
---

# Compact served / agentic / SDK agents — bound the unbounded conversation

## Goal
Give non-CLI agents a working compaction threshold. `maybe_compact` returns early when
`compact_threshold_chars == 0` (`crates/flux-flow/src/engine.rs:758`), and every non-CLI construction
path uses the default `0` (`crates/flux-app/src/app.rs:856` builds with `..AgentSpec::default()`;
`crates/flux-agent/src/lib.rs:158`). Only the CLI sets a non-zero value (`crates/flux-cli/src/main.rs:1721`,
`FLUX_COMPACT_CHARS`, default 48000). Because `run_agent` binds a conversation/thread to **one persistent
session** (`app.rs:709`, `session_for`), a D-09 agentic channel target re-sends the whole growing
transcript every turn — linear cost, then a hard provider context-window error.

## Acceptance
- [ ] Failing-first test (in `flux-app` or `flux-flow`): a served/agentic agent driven past the threshold
      **compacts** — assert the post-compaction message history shrank (or a compaction summary replaced the
      prefix), whereas today it grows unbounded.
- [ ] A compaction threshold is threaded into the app + SDK agent-spec construction (via `AgentDecl` /
      `AgentSpec` / builder), with a sane non-zero default for long-lived served agents and a per-agent
      override; the CLI env override keeps working.
- [ ] Default behaviour for a one-shot `flux run` is unchanged (no surprise compaction on short turns).
- [ ] Design note: chosen default, where configured, precedence (per-agent > env > default).

## Progress
- 2026-07-03 DONE — `AgentSpec::default().compact_threshold_chars` now `DEFAULT_COMPACT_THRESHOLD_CHARS=48_000` + `with_compaction`; `agent_spec_from_decl` precedence per-agent (`settings.compact_threshold_chars`) > `FLUX_COMPACT_CHARS` env > default; CLI + one-shot `flux run` untouched. Tests: `served_agents_get_a_nonzero_compaction_default`, `agent_spec_has_nonzero_compaction_default_and_per_agent_override`, `agent_past_threshold_compacts_the_conversation`. Full gate green.

## Notes
- Evidence: `crates/flux-flow/src/engine.rs:758`, `crates/flux-app/src/app.rs:856`,`:709`,
  `crates/flux-agent/src/lib.rs:158`, `crates/flux-cli/src/main.rs:1721`.
- Residual of the compaction seam / [D-09](D-09-agentic-channel-target.md). Design: [library-hardening](../designs/library-hardening.md).
