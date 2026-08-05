---
id: C-227
title: "A dropped provider stream ends the whole turn — no automatic resume for a transport-class failure"
pillar: Core
status: ready
priority: 19
epic: unattended-run-integrity
design: docs/designs/unattended-run-integrity.md
note: "`stream closed before completion` mid-exploration kills a 34-step run outright; flux has `--continue` but nothing retries, so long headless runs are a coin flip on provider transport"
---

# A dropped provider stream ends the whole turn — no automatic resume for a transport-class failure

## Goal
A transport-level provider failure — the socket closes mid-stream, an upstream rate-limit lands
between stages — is **not** a decision the agent made and not a task-level failure. Today it ends the
turn outright. A long agentic run that has executed dozens of ops and written real files loses the
rest of its work to one dropped TCP stream.

`flux` already has every piece needed to survive this: sessions are durable, and `flux run
--continue` resumes the most recent one. What is missing is anything that *invokes* that
automatically. Every caller has to hand-roll the same retry loop outside the process.

Combined with [C-226](C-226-failed-turn-is-indistinguishable-from-a-successful-one.md) — the failure
exits 0 and looks like success — the practical result is a run that stops early and reports nothing
wrong.

## Acceptance
- [ ] Transport-class provider failures are **classified distinctly** from declared provider
      failures. A 401, a refused model, a content-policy rejection, an exhausted token budget are
      task-level and must NOT be retried; a closed stream / connect timeout / 429-with-retry-after
      is transport-level and is a retry candidate. Name the classification seam explicitly — this
      split is the whole story, and retrying an auth failure in a loop is worse than not retrying.
- [ ] A **bounded** automatic resume for the transport class: capped attempt count, backoff, and
      `retry-after` honoured when the provider supplies one. Off-by-default vs on-by-default is a
      deliberate call to state in the story, not to leave implicit.
- [ ] The resume is **visible, never silent**: each attempt is surfaced on the human surface and as
      a typed line on the NDJSON stream, so a consumer can distinguish "one clean turn" from "one
      turn that needed four attempts". A silent retry that inflates cost with no trace is a
      regression in auditability, which is the property the whole runtime is built around.
- [ ] Retries are accounted: `usage`/`cost_usd` reflect **all** attempts, not just the one that
      succeeded.
- [ ] **Failing-first test**: a stub provider that closes the stream mid-turn on its first call and
      succeeds on the second. Assert the turn completes and reports the retry. It fails today —
      the first close ends the turn.
- [ ] Interaction with `--max-model-calls` / `--turn-budget` is decided explicitly: retries must not
      become a hole in the budget ceiling.
- [ ] Standard gate green in both workspaces.

## Progress
- 2026-08-04 — reproduced again in a live Flux session as
  `provider error: sse stream: connection closed after 67s`. This is the exact transport class this
  story owns: the elapsed duration is useful retry telemetry, but the closure still ended the turn
  instead of entering the bounded visible-resume path.
- 2026-07-29 — found driving `flux run` as a headless sub-agent implementor. Four runs against
  `openrouter/google/gemini-3.6-flash`, `gemini-3.5-flash` and `gemini-2.5-flash` all died mid-run
  (three on `stream closed before completion` at ~12k/14.7k/21.1k context, one on an upstream
  rate-limit). A hand-written wrapper looping `flux run --continue` on failure carried the same task
  from step 16 to step 34 and produced real edits — i.e. **resume works; only the automation is
  missing.** That wrapper is the evidence this belongs in flux rather than in every caller.

## Notes
- Seams: the provider stream error surfaces through `crates/flux-flow/src/loop_host.rs:574-586`
  (`explore`) as a stage `Err`; `--continue` / `--resume` are wired in `crates/flux-cli/src/args.rs`
  and dispatched from `crates/flux-cli/src/dispatch.rs`.
- The lower transport mapping currently formats event-source failures as `sse stream: {error}` in
  the Messages, OpenAI Chat and OpenAI Responses codecs. Preserve the structured source/cause and
  elapsed duration before the string is wrapped so retry policy does not depend on matching prose.
- ⚠ **Resume must not corrupt session shape.** Re-entering a turn after a partial stream is exactly
  the termination-path class AGENTS.md flags as having recurred three times: a resumed turn must not
  leave a split `tool_use`/`tool_result` pair or a user-after-user sequence. Only a live provider 400
  catches this; the mock provider will not.
- **Idempotency is the sharp edge.** If the stream dropped *after* an op executed but *before* its
  result was recorded, a naive resume can re-fire a side effect. Decide and state whether resume is
  safe for already-dispatched effects, or whether it must resume strictly at a stage boundary.
- Related: [C-228](C-228-gemini-3x-over-openrouter-drops-the-stream-mid-exploration.md) is the
  provider-side bug that made this failure common enough to find; this story is the runtime-side
  resilience that should hold regardless of which provider misbehaves.
