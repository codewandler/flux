---
id: C-587
title: "Every Fleet candidate produces independent review and structured reflection before handoff"
pillar: Improve
status: backlog
epic: agent-loop-harnesses
design: docs/designs/agent-loop-harnesses.md
areas: [flux-flow, flux-runtime, flux-orchestrate, flux-events, flux-cli]
depends_on: [C-570, C-572]
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
- 2026-08-08 — **the review half is implemented; the reflection half is not.** No box is ticked,
  because every Acceptance item above is a compound "review *and* reflection" claim and only one
  side of each is true. What now exists, in `crates/flux-cli/src/board_fleet_cmd.rs`:
  - `flux fleet review WAVE [--item ITEM] [--from FILE]` admits a **fresh read-only agent that is
    not the writer** (`admit_candidate_reviewer`) whose workspace is a per-candidate sandbox holding
    only its packet — no repository checkout, no fleet state, no writer conversation, no session
    inherited from the writer, and a `read-only` admission so it cannot edit what it judges.
  - The packet (`review_packet`, `flux.fleet-review-packet/v1`) is host-derived: story Goal and
    Acceptance read **at the reviewed commit**, the exact normalized diff, the observed write set,
    candidate/base identities and a diff digest.
  - Findings are structured and evidence-bound — closed `category`/`severity`/`confidence`
    vocabularies, `component`, and exactly one of `{path,line}` / `{command}` / `{invariant}` — with
    `source` separating a reviewer's assessment from a host-derived fact.
  - The verdict gates: `integrate_wave` refuses any candidate without a PASS at its exact handoff
    commit (`candidate_review_refusal`), and REWORK routes through the existing `fleet_rework`
    budget, so the third round still parks.
  - It fails closed. Receipts carry `examined` beside `verdict`, so a clean review and a review that
    never ran are different rows; a stopped fleet, a missing contract, a packet that does not fit, a
    failed turn and an exhausted retry run each record a distinct state and none of them passes.
  - `drive_one_tick` reviews every ready candidate, so an unattended fleet needs no operator call.
  - Receipts are keyed by wave, BoardRef, repository, attempt, reviewer id/session, loop and model,
    candidate digest and the writer's identity, appended to the story's `review_receipts`.
- Not implemented, and deliberately left: `AgentReflection/v1`, the bounded worker-transcript packet,
  the central improvement projection and `flux fleet insights` (Acceptance 3, 5, 7 and the reflection
  halves of 1, 2, 4, 6, 8, 9). The dedicated `review` loop profile and its authored strict-review
  protocol, the reviewer's `PARK` vocabulary, and the `candidate_ready` → `handoff_ready` two-receipt
  naming stay with C-572; this uses the configured `loop_policy["review"]` binding as it stands, and
  keeps `PARK` host-derived from the rework budget.

## Notes

- C-490 `/insights` remains an on-demand session facts+narration surface. This story may reuse its
  bounded/redacted packet builders, but mandatory Fleet reflection is a typed durable receipt.
- `crates/flux-eval` already has deterministic pain-point mining and structured external review. Reuse
  its taxonomy/aggregation semantics where they fit instead of creating incompatible improvement
  JSON, while keeping Fleet state out of the eval crate.
