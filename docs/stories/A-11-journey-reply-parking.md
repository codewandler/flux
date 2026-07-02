---
id: A-11
title: Reply-parking for `ask` — journeys suspend until the correlated reply arrives
pillar: Agent
status: done
priority: 5
note: a top-level `$reply = ask(...)` is lowered at journey-run time into ask + `await` (source ask.reply) — the flow suspends on the EXISTING seam, the App parks it keyed by asked channel, and a correlated inbound message (channel name, or user_input for CLI channels) is CONSUMED to resume the oldest matching park via resume_flow with the reply bound; zero flux-flow/flux-lang changes, envelope invariant intact
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
- **Done (2026-07-02).** Design: an op result can't suspend a flow, so the host **lowers** every
  top-level `$reply = ask({channel, message})` at journey-run time into the same (unbound) `ask`
  call followed by `Node::Await { binding, source: "ask.reply" }` — the interpreter suspends
  exactly as for a hand-written `await`, the App persists the resume point
  (`FlowStore::save_suspension` on the run's own store) and parks it (`src/park.rs`:
  `ParkedAsk`, keyed by the asked channel resolved from the bus's expects-reply sends).
  - **Correlation rule (documented in park.rs):** an inbound event resumes the OLDEST pending park
    it matches — label == asked channel name, or label == `user_input` for CLI-rendered channels
    (the `flux app run` stdin loop). A correlated event is **consumed** (doesn't also fire
    triggers); uncorrelated events route normally. No explicit correlation-id matching — channels
    deliver plain messages with nowhere reliable to carry an id.
  - Resume re-enters via `flux_flow::runtime::resume_flow(_with_composites)` over a fresh
    full-envelope Executor — no side-channel execution; re-parks if the continuation asks again.
  - Tests (failing-first — the suspend test failed with the old `"ask:cli"` fire-and-forget
    result): the 3 story tests + 3 park unit tests (lowering shape, nested-ask untouched,
    correlation matrix). Package gate + codegate green.
- **Residuals:** park timeout/expiry (out of scope per story); parks are in-memory (matches the
  app's in-memory store posture); NESTED asks (inside when/repeat/parallel) keep fire-and-forget —
  `await` is top-level-only in flux-lang; a `spawn`ed child that asks returns its (empty) parked
  result to the parent immediately; hand-written top-level `await` in a journey still has no
  flux-app resume surface (pre-existing); the lowering drops the bind's `effect` annotation
  (its `ty` rides as the await's `as_type`).

## Notes
- Envelope invariant: the resume path re-enters through the normal engine/executor path — no
  side-channel execution.
