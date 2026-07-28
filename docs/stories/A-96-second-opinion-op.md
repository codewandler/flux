---
id: A-96
title: Second-opinion op — consult a different model for advice, never effects
pillar: Agent
status: backlog
priority:
epic:
design:
note: "every escalation path today carries authority (sub-agents are policy-bounded but still act); a PURE consult op adds no new authority to the envelope at all — provider/model routing already exists (args.rs:82-97), so this is a read-only op over machinery that ships"
---

# Second-opinion op — consult a different model for advice, never effects

## Goal
Give the agent one cheap move for a hard sub-question: ask a *different* model — typically a
stronger or differently-biased one — and get back **advice**, not actions. The op is pure: it takes
a question plus caller-supplied context, performs exactly one model call, and returns text. It
cannot read, write, spawn, or reach the network beyond that call, so it adds **zero new authority**
to the safety envelope — the cheapest possible fit for flux's thesis, and the surface where
provider neutrality pays off directly (the second opinion can come from a different vendor).

## Acceptance
- [ ] A pure `consult` op (name to be settled) accepts a question + context and a `provider/model`
      spec, performs one model call, and returns the answer as text — failing-first test driving
      `-m mock` and asserting no effect is declared beyond the model call.
- [ ] The op declares `Effect`/`Risk`/`Idempotency` honestly as a **non-mutating** operation and is
      pinned by test as carrying no filesystem, process, or network authority — it cannot be used
      as an egress channel by construction, because the only outbound path is the configured
      provider.
- [ ] The consulted model is resolved through the existing `provider/model` routing
      (`args.rs:82-97`), including the subscription providers, and falls back to the agent's model
      when unspecified.
- [ ] Its cost is attributed to the calling turn: the call emits `call_usage` like every other
      model call, so `flux usage` and the turn's cost line include it. Test asserts the usage event.
- [ ] The returned text enters context as **untrusted content** — it is model output from
      elsewhere and must not be able to close a containment tag (the A-21 lesson). Test covers a
      hostile answer containing the containment delimiter.
- [ ] Surfacing follows the existing rules — the op is not advertised unconditionally if that would
      churn the prompt (see A-95).

## Progress
- (not started — filed from the 2026-07-28 Amp feature-mining pass)

## Notes
- Source: [../research/amp.md](../research/amp.md) — Amp's **Oracle** tool (a separate,
  higher-reasoning model consulted for hard problems), which its manual singles out as
  high-value and recommends invoking explicitly.
- The distinction from sub-agents is the whole point: a sub-agent is a bounded *actor* (it has
  tools, a policy scope, and a workspace); this is a bounded *adviser* with no tools at all.
- Open question worth deciding in the story, not at implementation time: is this model-invoked
  (the agent decides to consult) or user-invoked (`/consult`), or both? Model-invoked is where the
  value is, but it is also where the cost is — so it may want a per-turn call cap.
- Cost interaction: a second opinion is by definition a cold prompt for the consulted model. Expect
  no cache benefit and price it accordingly (see the C-133…C-140 cache work).
