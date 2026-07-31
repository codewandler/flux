# Serving surface and turn outcome residuals — 2026-08-01

## Context

`SRV-01`, `SRV-02` and `OUTCOME-01` all validate as **fixed in the subsystem the reviewers read**
and **open one layer out**.

- **SRV-01** — the REST SSE stream is now a bounded 256-slot channel with a request-owned
  `CancellationToken`, a drop guard that fires when the response body is dropped, and a
  full-buffer cancel. `dropping_rest_sse_body_cancels_and_finalizes_the_turn` was executed during
  validation and passes; it cannot pass against a detached task.
- **SRV-02** — `flux-server` now has a real cardinality-bounded `ResourceGovernor`: 120 req/min,
  4 in-flight, 1000 provider calls/day, $25/day, principal-keyed, `429` + `Retry-After` +
  `X-Flux-Limit`. P's Low severity was right about the daemon.
- **OUTCOME-01** — provider-stage failures are carried to `TurnTerminal` and converted to a hard
  `Err`; the end-to-end test spawns the real binary and asserts a nonzero exit with a typed error line.

But: A/B's Medium is now right about a surface nobody inventoried. The `webhook` and `connector`
**channel adapters** mount a bare `Router::new()` — no body limit, no timeout, no rate guard, no
concurrency permit, no provider budget — and `tokio::spawn` the delivery *before* admission, so a
burst parks unbounded tasks behind a process-global semaphore. And the turn-gate is one mutex per
engine, so per-principal concurrency does not isolate the actually scarce resource.

On the outcome side, `turn_end.outcome` is a two-valued `ok|error` projection of a seven-valued
durable vocabulary: `suspended`, `max_iter`, `cancelled` and a denied approval all reach an
automation client as `"outcome":"ok"` with exit 0 — the C-226 failure mode, one branch over.

## Finding-to-story traceability

| Residual | Story |
| --- | --- |
| `webhook`/`connector` adapters have no limit layers and spawn before admission | C-370 |
| Queue depth is unbounded everywhere; `FlowEngine::turn_gate` is process-global and head-of-line-blocks across principals | C-371 |
| SSE turns have no wall-clock ceiling, can park on the gate forever, and `tasks/resubscribe` takes no permit; public card/health routes are unrated | C-372 |
| `turn_end.outcome` collapses `suspended`/`max_iter`/`cancelled`/denied to `ok` | C-373 |
| `carry_stage_failure` is silent-fallthrough and first-failure-wins; gather-call failures never reach the outcome; the E2E test covers the intent stage only | C-374 |

## Decisions

- **The limit contract belongs to the ingress, not to `flux-server`.** Any surface that accepts
  outside bytes and starts a turn is governed, or it is not exposed. A `.flux` program declaring a
  `webhook` channel and served by `flux app run` is an exposed daemon.
- **Admit before spawn.** A permit acquired inside the delivery bounds the work, not the queue.
- **A per-principal concurrency limit must bound the resource that is actually scarce.** While one
  global mutex serializes every turn, four in-flight per principal is an accounting fiction.
- **The protocol reports the durable outcome.** A consumer that cannot distinguish "parked on an
  approval" from "finished successfully" has the exact defect C-226 closed. Exit codes follow.
- **A best-effort failure carry is a latent regression.** `carry_stage_failure` requires both `kind`
  and `text`; a stage that tags differently silently reverts to laundering, guarded only by a
  `debug_assert` that release builds compile out.

## Closure proof

Load-test each ingress class with valid credentials and assert a typed limit response; drive a
suspended, a max-iter, a cancelled and a denied turn through the NDJSON stream and assert the
outcome and exit code differ from success.
