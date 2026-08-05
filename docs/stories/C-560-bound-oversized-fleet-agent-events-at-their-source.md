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
