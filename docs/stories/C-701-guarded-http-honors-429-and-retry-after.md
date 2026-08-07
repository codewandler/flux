---
id: C-701
title: "Guarded HTTP honors 429 and Retry-After"
pillar: "Core"
status: in-progress
epic: first-class-hosts
areas: [flux-web]
design: docs/designs/the-substrate-seam.md
note: "flux-web has no 429 or Retry-After handling at all, while flux-provider/src/retry.rs already treats 429 as retryable for provider calls — the guarded family is the odd one out"
---

# Guarded HTTP honors 429 and Retry-After

## Goal

`http.request` and `web.fetch` hand a 429 straight back to the caller. There is no `Retry-After`
handling anywhere in `flux-web`, while `flux-provider`'s retry machinery already classifies 429 as
retryable for provider calls — so the one HTTP family an agent actually drives against third-party
APIs is the one that gives up first, and every authored program that talks to a rate-limited
service has to reimplement backoff itself.

**Why this is safe to retry even now:** a 429 is a *definite answer*. The far side received the
request, rejected it, and said when to come back. That is categorically different from an
`Unreachable`, where the request may or may not have been processed — which is why C-674's framed
HTTP route carrying no at-most-once guarantee does not block this. A 429 retry is sound for any
method, including a POST, precisely because the server is telling you it did not act on it.

The retry belongs where the request is *made* — in the egress client behind `GuardedHttp` — so a
selected remote substrate retries next to the target it is calling rather than across the wire, and
local and remote behave identically without the wire learning anything new.

## Acceptance

- [ ] A 429 is retried honoring `Retry-After` in both its forms (delta-seconds and HTTP-date), with
      a bounded attempt count and a bounded total wait, defaulting conservatively; a response with
      no `Retry-After` uses bounded exponential backoff with jitter.
- [ ] Retrying never exceeds the request's existing wall-clock budget: the budget bounds the whole
      chain including waits, and a retry that would overrun it returns the 429 rather than blocking
      past it. The wait is cancellable — a cancelled turn does not sit in a sleep.
- [ ] Each hop of the redirect chain and each retry re-runs the guard and the per-hop secret
      re-authorization exactly as a first attempt does; a retry may not reuse a decision made for a
      previous attempt.
- [ ] Retries are visible rather than silent: the response records how many attempts were made and
      how long was spent waiting, and the operator-facing surface can say "rate-limited, retried N
      times" rather than presenting unexplained latency.
- [ ] Whether 503-with-`Retry-After` joins 429 is decided deliberately and justified in the story or
      the code — it is a "come back later" signal too, but unlike 429 it does not promise the
      request was not acted on.
- [ ] Tests drive: a 429 with delta-seconds, a 429 with an HTTP-date, a 429 with no header, budget
      exhaustion mid-wait, cancellation mid-wait, and a retry that must re-authorize a scoped
      secret.


## Comments

- In progress: dispatched to an implementor in worktree flux-c701 off base f1c1bf09. Retry lives in the egress client behind GuardedHttp so a selected remote substrate retries next to the target and the wire learns nothing new. Safety argument to preserve: a 429 is a definite answer, so retry is sound for any method including POST — which is why C-674's framed route carrying no at-most-once guarantee does not block it.
