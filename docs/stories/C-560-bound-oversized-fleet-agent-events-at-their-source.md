---
id: C-560
title: "Bound oversized Fleet agent events at their source"
pillar: "Agent"
status: backlog
priority: 30
areas: [flux-cli, flux-runtime]
note: "Research why a read-only three-repository Fleet task produced a 1.23 MB model request/event; define per-event and accumulated context bounds before optimizing transport"
---

# Bound oversized Fleet agent events at their source

## Goal

Explain why a bounded read-only Fleet inspection produced a 1.23 MB model request and an oversized
stream event, then establish evidence-backed size limits at the source so ordinary coordinator turns
cannot grow with whole repository files, repeated tool history or unbounded repair context.

## Acceptance

- [ ] A hermetic failing fixture reproduces the 2026-08-05 shape: a continued Fleet main session,
      three configured repository read roots, repeated exploration/repair rounds and a stream event
      larger than the supervisor capture window. It requires no provider credential or network.
- [ ] Instrumented byte accounting attributes the request and emitted event to prompt layers,
      conversation history, tool schemas, tool results, execution reports and repair attempts. The
      evidence distinguishes model-request bytes, NDJSON event bytes, durable session-event bytes
      and the Fleet receipt rather than treating them as one payload.
- [ ] The investigation explains the observed `message_bytes = 1,231,783`, 28-operation catalogue,
      seventh explore round and sixth repair attempt from the recorded read-only task, including
      whether `--continue` retained avoidable prior Fleet output or repository contents.
- [ ] A reviewed design fixes hard per-result, per-event, per-request and accumulated-turn bounds,
      with explicit summaries/references for omitted content. No layer silently slices structured
      JSON, loses the terminal outcome or re-expands already bounded evidence.
- [ ] Regression tests prove a normal cross-repository Fleet inspection stays below the accepted
      budgets, an adversarial oversized result remains inspectable, and continued sessions do not
      grow linearly with repeated copies of prior tool output.
- [ ] The operator-facing diagnostics expose which budget was approached or exceeded without
      persisting repository contents, secrets or an unbounded payload. Any implementation work that
      does not fit this story is filed as an explicitly linked follow-up before the research closes.


## Evidence

- 2026-08-05 Fleet research at fleet revision 18. Evidence: execute_agent_turn_with_runtime resumes with --continue; staged adaptive exploration sends state.messages.clone() on every round, then appends assistant output and repair prompts; round 7 and repair attempt 6 follow directly from zero-based explore_calls; ModelCallMetrics measures message_bytes separately from system and tool-schema bytes; Fleet parses child stream lines into an unbounded Vec<Value>. Leading inference: repeated full adaptive-history replay is the primary source of the 1,231,783 message bytes, with continued-session retention a likely fixed prefix; 28 operation schemas cannot explain that metric. Proposed hermetic fixture: six scripted roughly 200 KiB tool results followed by a seventh explore request, with 28 lightweight operations and resume enabled. Candidate budgets for design review: 64 KiB per model-history tool result, 256 KiB per NDJSON event, 512 KiB continued history, and 1 MiB total provider request. The receipt-amplification path remains unproven and needs the next bounded source pass.
