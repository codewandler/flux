---
id: C-701
title: "Guarded HTTP honors 429 and Retry-After"
pillar: "Core"
status: backlog
priority: 2
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

## Design

### Where the retry sits

In the egress client, in `NativeHttp::send` (`crates/flux-web/src/native_http.rs`), wrapped **around**
the whole attempt rather than inside `egress::send_guarded`. The reason is structural: the first
hop's secret re-authorization happens above the redirect chain, so a loop wrapped around
`send_guarded` would leave that check outside itself and every retry would ride on the decision the
first attempt made. Wrapping the attempt means each retry re-mints the guarded target through
`flux_system::net::guard_url_scoped_for_secret`, re-authorizes every carried secret against the
destination *that* admission produced, and only then re-enters the redirect chain — which re-admits
and re-authorizes each hop as it always did. The first attempt still uses the target the caller
minted, so an ordinary request's DNS behaviour is unchanged.

Because it lives in the client, a selected remote substrate runs the loop on the serving machine:
verified end to end in `flux-server`'s
`a_remote_substrate_retries_a_429_on_its_own_side_and_reports_what_it_cost`, where the origin — a
listener only the daemon can reach — is contacted twice while the coordinator makes one framed
request and gets one answer. The wire learns nothing new beyond two counters on the response frame.

The wait is `tokio::time::sleep(..).await`, never a blocking sleep, so cancellation (which in this
codebase is the caller dropping the operation future) ends it at once and leaves the retry unsent.

### Bounds

Three retries, a single wait capped at 30s, a total wait capped at 60s, and — the tighter bound in
practice — the request's own wall-clock budget, which must still have a second of headroom left
after the wait. A retry that cannot fit returns the 429, because a 429 is *data* and turning it into
a timeout error would be strictly worse for the caller. A `Retry-After` asking for longer than 30s is
not honoured by waiting less: the 429 goes back with its header intact and an authored program
decides.

### 503

**Not retried.** The Goal's safety argument is that a 429 is a definite answer — the far side
received the request and declined to act on it — which is what makes retrying sound for any method
including a POST. A 503 makes no such promise: a gateway answers 503 when an upstream became
unreachable, which can happen *after* the request was forwarded, and a server can answer 503 part-way
through handling one. That is the `Unreachable`-shaped uncertainty the port already refuses to paper
over, and this family carries requests whose method a model chose, so no caller here can promise
idempotence on its behalf. `flux-provider` does retry 5xx, and the asymmetry is intended: its peer is
one known completions endpoint whose calls the caller already treats as replayable. Narrowing 503 to
GET and HEAD would be defensible and the door is open, but a policy that silently differs by method
is a worse default than one a reader can state in a sentence.

### Jitter without a dependency, and without flaky tests

`retry::wait_after` is a pure function of an `Attempt` — jitter included, as an input. The only
nondeterminism in the module is `retry::jitter`, which reads sub-second clock skew (data the process
already has; no crate, no `Math.random`) and is bounded to `[0, 250ms]`. So the schedule is asserted
*exactly* in unit tests, and the live tests only bound a range that jitter can widen but not unbound.

### Reuse of `flux-provider`

Not shared. `flux-provider` is L1 and `flux-web` L5, so the layer map permits the dependency — but
its `retry_delay`/`retry_after_ms` are private, `retry_after_ms` handles only delta-seconds (no
HTTP-date), it retries 5xx (deliberately not wanted here), and adding `flux-provider` to
`flux-web`'s manifest would make the web pack depend on the model-provider crate for a fifty-line
policy. The shape is matched instead — same 500ms doubling backoff, same bounded additive jitter,
same "cap so an untrusted header cannot stall a turn" rule — and stated in `crates/flux-web/src/retry.rs`.
