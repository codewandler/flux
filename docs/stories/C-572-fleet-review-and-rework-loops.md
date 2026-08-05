---
id: C-572
title: "Fleet review and rework run under explicit reviewer and repair loops"
pillar: Core
status: backlog
epic: agent-loop-harnesses
design: docs/designs/agent-loop-harnesses.md
areas: [flux-flow, flux-runtime, flux-orchestrate, flux-cli]
depends_on: [C-567, C-570]
note: "fresh read-only reviewer loop over exact commit; typed PASS/REWORK/PARK; repair resumes the writer loop and the host keeps the two-round ceiling"
---

# Review through a loop without letting the writer review itself

## Goal

Make Fleet review and repair explicit loop profiles while preserving fresh reviewer independence,
typed findings, same-writer rework and host-enforced evidence.

## Acceptance

- [ ] Failing first, the Fleet workhorse can hand off but review behavior is selected by general
      agent defaults rather than an admitted reviewer binding. The fixed state machine starts one
      fresh read-only reviewer at the exact handoff commit and snapshots its loop/capability/budget.
- [ ] The reviewer loop invokes or composes the shipped `strict_review` Flux flow, inspects only the
      admitted commit/context and returns typed `PASS`, `REWORK(findings)` or `PARK(findings)`.
      Malformed output and review gaps never become PASS.
- [ ] The writer reports `handoff_ready` before review begins. Reviewer context contains no writer
      conversation and reviewer capabilities cannot write; the writer never applies its own review
      result.
- [ ] `REWORK` continues the original writer session at the workhorse loop's explicit `repair`
      entry point with structured findings and reviewed-commit identity. The existing two-delivery
      C-245 budget and third-attempt PARK remain durable host invariants.
- [ ] Reviewer and repair reports carry binding/commit/evidence references with bounded payloads.
      Host verification still owns write sets, test commands, commits, integration and final gate.
- [ ] Restart, duplicate review result, stale commit, cancellation, reviewer failure and repair yield
      are deterministic and preserve inspectable evidence without creating a second writer.
- [ ] The five-writer, three-repository dogfood completes fresh reviews and any required rework under
      the recorded loop bindings before C-565 is marked done.

## Progress

- (not started)

## Notes

- This is how “review belongs in the loop harness” composes with Decision 0010: review is a distinct
  fresh agent loop in the Fleet pipeline, not a self-review phase inside the writer.
