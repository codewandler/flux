---
id: C-229
title: "Unattended run integrity — survive provider transport failure, and be honest when you don't (epic)"
pillar: Core
status: ready
priority: 16
epic: unattended-run-integrity
design: docs/designs/unattended-run-integrity.md
note: "three separately-filed stories are one failure at three depths — a provider bug, no resume, and no way to tell a dead turn from a live one; C-228 must be DIAGNOSED before C-227 is designed, or the retry masks a deterministic codec bug as a flaky network"
---

# Unattended run integrity — survive provider transport failure, and be honest when you don't (epic)

## Goal
C-226, C-227 and C-228 were filed separately and describe **one failure at three depths**: a provider
stream that dies mid-run (C-228), no way to resume from it (C-227), and no way for any caller to tell
that it happened (C-226).

Together they say something none of them says alone: **flux is not currently safe to run unattended
against a real provider for a long task** — and the reason nobody has quantified that is C-226. The
failures are invisible to exactly the automated consumers who would otherwise have counted them. A
turn that never ran exits `0`, emits no NDJSON `error` line, and reports the failure as prose inside
`turn_end.answer`.

## Acceptance
- [ ] C-226 (typed turn outcome + non-zero exit), C-227 (bounded, visible, accounted resume for
      transport-class failures) and C-228 (the Gemini-3.x stream death) are done, each with the
      failing-first test its story names.
- [ ] **C-228 is diagnosed before C-227's retry is designed** — see Notes. Its finding is recorded
      either way, including "could not reproduce" with the reproduction attempt described.
- [ ] A long unattended run against a flaky provider either completes — having retried a transport
      failure a bounded, visible, **accounted** number of times — or exits non-zero with a typed
      error a subprocess driver can branch on without parsing prose.
- [ ] Retries never become a hole in the spend ceiling: the interaction with `--max-model-calls` /
      `--turn-budget` is decided explicitly, and `usage`/`cost_usd` reflect **all** attempts.
- [ ] `openrouter/google/gemini-3.x` is either usable for sustained agentic runs, or documented as
      not — where a user *choosing a model* will see it, not mid-task.

## Progress
- 2026-07-29 — epic opened over three stories that were already filed and `ready` with an empty
  `epic:` field. Design: [unattended-run-integrity.md](../designs/unattended-run-integrity.md). Every
  claim was verified against the tree before filing: the doubled laundering site, the existing
  envelope corpus, the live `StreamDiagnostic` infrastructure, and the NDJSON `error` line.
- Sequence: **C-226 ∥ C-228, then C-227.** The first two are file-disjoint (`flux-flow`/`flux-cli`
  vs `flux-providers`) and can run in one wave; C-227 lands last because it depends on both.

## Notes
- **The load-bearing decision: C-228 is a diagnosis, and it gates C-227's design.** C-227 proposes
  retrying transport-class failures. C-228's open question is whether the stream close *is* a
  transport event, or whether flux's own codec ends the stream on an unhandled reasoning envelope.
  If it is the latter, **retrying re-runs a deterministic bug** — every attempt fails identically at
  the same context depth, burning the retry budget and real money, and converting a reproducible,
  fixable defect into what looks like a flaky network. That is strictly worse than today's honest
  hard failure.
- **The evidence already leans that way.** `gemini-2.5-flash`, which has **no reasoning stream**,
  survives the same workload that reproducibly kills `gemini-3.5-flash` and `gemini-3.6-flash`. A
  vendor-wide transport problem would not discriminate by whether a model emits reasoning deltas.
- **There is an invariant to check it against.** A-33…A-37 established that codecs *skip and count*
  an unparseable SSE/frame envelope, surfacing `Chunk::StreamDiagnostic` at stream end, rather than
  `?`-propagating. A hard `api_error` that kills a turn is precisely the shape that invariant exists
  to prevent. So if the reasoning envelope is being **rejected rather than skipped, C-228 is an
  invariant regression, not a feature request** — a bug in flux, with a home for its fixture already
  in `crates/flux-providers/src/envelope_corpus.rs`.
- **C-227's classification seam is its whole substance.** A 401, a refused model, a content-policy
  rejection, an exhausted token budget are task-level and must never be retried. Retrying an auth
  failure in a loop is worse than not retrying: it converts one clear error into a delayed one, after
  spending the budget.
- **C-226 must not regress the human surface.** The apologetic answer text stays; the machine-readable
  outcome lands *alongside* the prose. And `usage: null` + `cost_usd: null` is not an acceptable
  substitute contract — it is a coincidence of the current failure path, and a consumer keying on it
  misclassifies the first failure carrying partial usage.
- Non-goals: no general-purpose retry framework, no change to task-level failure handling, no new
  cross-provider transport abstraction. See the design doc.
