---
id: A-11
title: Reply-parking for `ask` — journeys suspend until the correlated reply arrives
pillar: Agent
status: ready
priority: 5
note: `ask` today is `send` + a correlation id (the reply is never awaited — the in-code TODO at flux-app ops.rs); wire it onto the existing suspension seam so a journey parks on ask and App::deliver resumes it with the reply
---

# Reply-parking for `ask` — journeys suspend until the correlated reply arrives

## Goal
`flux-app`'s `ask { channel, message }` op is documented as expecting a reply but actually behaves
like `send` and immediately returns `ask:<channel>` — "full request/response correlation (parking
the journey until a reply arrives) is a TODO" (`crates/flux-app/src/ops.rs:171,207`). Implement the
parking: an `ask` suspends the journey; a later inbound message correlated to it resumes the flow
with the reply text bound as the op's result.

## Why
Channel journeys (D-04) that need an answer (approval flows, slot-filling over Slack) currently
have no way to wait — the conversation state lives only in prose. flux already owns the right
primitive: flow suspension + resume (`FlowStore` suspensions, `resume_suspended`, used by
`confirm`). `ask` should ride the same seam, not invent a parallel one.

## Acceptance
- [ ] **`ask` suspends.** In a journey, `ask` returns the suspension outcome (same mechanics as
      `confirm`), parking the flow keyed by a correlation id. Failing-first test:
      `ask_suspends_the_journey_until_a_reply`.
- [ ] **Correlated resume.** `App::deliver` of an inbound message carrying the correlation id (or
      arriving on the asked channel while a park is pending — pick the correlation rule and
      document it) resumes the parked journey; the op's bound result is the reply text. Test:
      `delivered_reply_resumes_with_the_reply_text_bound`.
- [ ] **Uncorrelated messages don't resume.** A message for another channel/correlation starts or
      routes normally and the park stays parked. Test:
      `unrelated_message_does_not_resume_the_parked_journey`.
- [ ] **CLI channel**: on a `cli` channel the printed question + the next `App::deliver` (stdin
      line in `flux app run`) resolves the ask — keeping the current interactive demo working.
- [ ] Gate green: `cargo test --workspace`, clippy `-D warnings`, fmt, `cargo test -p flux-codegate`.

## Progress
- (not started — filed 2026-07-02 from the in-code TODO during the ready-queue curation.)

## Notes
- Timeouts/expiry of a park are a sensible follow-up but NOT required here; record as residual if
  skipped.
- Envelope invariant: the resume path must re-enter through the normal engine/executor path — no
  side-channel execution.
