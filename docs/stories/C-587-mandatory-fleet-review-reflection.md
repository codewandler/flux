---
id: C-587
title: "Every Fleet candidate produces independent review and structured reflection before handoff"
pillar: Improve
status: backlog
epic: agent-loop-harnesses
design: docs/designs/agent-loop-harnesses.md
areas: [flux-flow, flux-runtime, flux-orchestrate, flux-events, flux-cli]
depends_on: [C-567, C-570, C-572]
note: "after implementation freezes, async tool-free review(story+diff) and reflection(request+transcript+outcome) are mandatory; reflections feed a central improvement corpus"
---

# Every Fleet candidate produces independent review and structured reflection before handoff

## Goal

Make every Fleet implementation attempt teach the system something: run independent code review and
harness reflection concurrently after the candidate is frozen, require both receipts before final
handoff, and collect structured friction centrally for self-improvement.

## Acceptance

- [ ] Failing first, a Fleet writer can currently reach handoff with no mandatory reflection and no
      central structured record of context, tool or instruction friction. The fixed workhorse reports
      `candidate_ready`; only the host may derive `handoff_ready` after the two-receipt barrier.
- [ ] The host starts two fresh, independently budgeted, tool-free agents concurrently for every
      candidate attempt: C-572 review receives only story Goal/Acceptance plus exact normalized diff;
      reflection receives the original assignment request, redacted bounded worker transcript and
      host-verified outcome/evidence summary. Neither receives the other's output or writer context.
- [ ] `AgentReflection/v1` has bounded common prose plus structured observations for context quality,
      missing information, missing/awkward tools, ambiguous/conflicting instructions, loop/harness
      friction, budget pressure and proposed improvements. Each observation carries category,
      severity, confidence, affected component and evidence references rather than copied transcript.
- [ ] Host-derived facts—tool errors/not-found, retries, truncation/omission, budget exhaustion,
      approvals, tests and terminal outcome—remain distinct from model assessment. Reflection cannot
      set PASS/REWORK/PARK, Board status, evidence truth or worker success.
- [ ] The transcript is complete when it fits the declared packet cap. Deterministic bounded
      selection otherwise records omitted ranges/bytes and `context_complete: false`; credential
      redaction and secret-output exclusion occur before the model call, and raw transcript text is
      not duplicated into the central corpus.
- [ ] The host durably records review and reflection states/receipts keyed by Fleet, wave, BoardRef,
      attempt, worker/session, loop/model and candidate digest. Malformed, failed, missing or
      unpersisted reflection retries within a bound and then produces typed attention/PARK rather than
      silently allowing handoff.
- [ ] A central bounded improvement projection deduplicates and aggregates structured observations
      across successful, REWORK and PARK attempts. `flux fleet insights` and JSON expose categories,
      trends, affected components and receipt/evidence references without raw prompts/transcripts;
      export composes with existing `painpoints_collect`/`improvements_aggregate` machinery.
- [ ] Restart, duplicate result, stale candidate, cancellation and rework are deterministic. Each new
      candidate digest gets a new paired attempt; the original writer alone repairs, the two-rework
      ceiling remains host-enforced and reflection cannot create another writer.
- [ ] A hermetic multi-worker fixture proves review and reflection overlap, final handoff waits for
      both, one reflection failure blocks only its assignment, every terminal attempt enters the
      improvement projection and usage is attributed to the correct BoardRef/attempt.
- [ ] Public Fleet/self-improvement docs, schemas, generated skills, CLI/TUI projections and
      changelogs distinguish `/insights` narration, review findings and mandatory Fleet reflections;
      the full repository gate and embedded-doc freshness gate pass.

## Progress

- 2026-08-05 — contracted from operator feedback after the explicit workhorse/reviewer loop plan.

## Notes

- C-490 `/insights` remains an on-demand session facts+narration surface. This story may reuse its
  bounded/redacted packet builders, but mandatory Fleet reflection is a typed durable receipt.
- `crates/flux-eval` already has deterministic pain-point mining and structured external review. Reuse
  its taxonomy/aggregation semantics where they fit instead of creating incompatible improvement
  JSON, while keeping Fleet state out of the eval crate.
