# Unattended run integrity — surviving provider transport failure, and being honest when you don't

**Epic:** C-229 · **Stories:** C-226, C-227, C-228 · **Status:** designed, none started

## The arc

Three stories filed separately describe one failure, at three depths:

- **C-228 — the symptom.** `openrouter/google/gemini-3.x` dies with `provider error: api_error:
  stream closed before completion`, reproducibly, part-way through exploration at 12–21k ctx. Short
  turns survive, which is why no smoke test catches it.
- **C-227 — the missing capability.** A closed socket is not a decision the agent made, yet it ends
  the turn outright. A run that has executed dozens of ops and written real files loses the rest of
  its work to one dropped TCP stream. flux has `--continue`, but nothing retries.
- **C-226 — the blindness.** The turn that just died exits **0**, emits no NDJSON `error` line, and
  reports the failure as prose inside `turn_end.answer`. No subprocess driver — CI, an editor
  extension, a coordinator fanning work to sub-agents — can tell it apart from success.

Read together they say something sharper than any of them alone: **flux is not currently safe to run
unattended against a real provider for a long task**, and the reason nobody has quantified that is
C-226 — the failures are invisible to exactly the automated consumers that would have counted them.

## The one ordering constraint that matters

**C-228 must be diagnosed before C-227 is designed.** This is the epic's load-bearing decision.

C-227 proposes retrying transport-class failures. C-228's whole open question is whether the stream
close *is* a transport event, or whether flux's own codec ends the stream on an unhandled reasoning
envelope. Those demand opposite responses:

- If OpenRouter really closes the socket, a bounded retry is the right fix.
- If flux's Messages-path codec rejects a reasoning delta instead of skipping it, then **retrying
  re-runs a deterministic bug**. Every attempt fails identically at roughly the same context depth,
  the run burns its retry budget and real money, and the retry machinery converts a reproducible,
  fixable defect into what looks like a flaky network. That is strictly worse than today's honest
  hard failure.

The evidence already points at the second: `gemini-2.5-flash`, which has **no reasoning stream**,
survives the same workload that kills `gemini-3.5-flash` and `gemini-3.6-flash`. A vendor-wide
transport problem would not discriminate by whether the model emits reasoning deltas.

There is a standing invariant to check it against. A-33…A-37 established that **codecs skip and count
an unparseable SSE/frame envelope, surfacing `Chunk::StreamDiagnostic` at stream end, instead of
`?`-propagating**. `StreamDiagnostic` is live infrastructure (9 uses in `flux-providers`), and
`crates/flux-providers/src/envelope_corpus.rs` (395 lines) is the existing home for exactly this kind
of regression fixture. A hard `api_error` that kills a turn is the shape that invariant exists to
prevent — so if the reasoning envelope is being *rejected* rather than *skipped*, C-228 is not a new
feature request, it is an **invariant regression**, and it is a bug in flux.

## Sequence

**C-226 ∥ C-228, then C-227.** Two independent starts, one dependent finish.

- **C-226** and **C-228** are file-disjoint and can run together: C-226 lives in
  `flux-flow`/`flux-cli` (the outcome contract and exit code), C-228 in `flux-providers` (the codec).
- **C-227 lands last**, because it depends on both:
  - on **C-228** for whether "transport-class" is even the right classification for the motivating
    case, and
  - on **C-226** for its own acceptance — "the resume is visible, never silent … a typed line on the
    NDJSON stream" is unsatisfiable until there *is* a typed outcome vocabulary. Building the retry
    first would mean emitting retry telemetry into a stream that still cannot express failure.

## The design questions each story must answer, and why they are hard

**C-226 — where does the typed outcome live?** The laundering site is explicit and doubled:
`crates/flux-flow/src/loop_host.rs` converts `Err(error)` into `Ok(json!({"kind": "error", …}))` for
both `detect_intent` and `explore`. The `kind: "error"` payload is therefore *already* the signal —
carry it rather than recomputing it. Two traps:

- **`usage: null` + `cost_usd: null` is not a substitute contract.** It is a coincidence of the
  current failure path. A consumer keying on it silently misclassifies the first failure that happens
  to carry partial usage.
- **The NDJSON vocabulary must not disagree with itself.** `stream_json.rs` already emits a
  `"type": "error"` line. Either a provider/flow-level turn failure fires it, or `turn_end.outcome`
  is declared the sole contract and the `error` line stays reserved for `run_turn`'s `Err(_)`. Both
  are defensible; having the two disagree is not.

And the human surface must not regress: keep the apologetic answer text. The goal is a machine-
readable channel *alongside* the prose, not a raw stack trace where a sentence used to be.

**C-227 — the classification seam is the whole story.** A 401, a refused model, a content-policy
rejection, an exhausted token budget are task-level and must **never** be retried; a closed stream,
a connect timeout, a 429 with `retry-after` are transport-level. Retrying an auth failure in a loop
is worse than not retrying at all — it turns one clear error into a delayed one, having spent the
budget. Three further calls the story must make explicitly rather than by default: on-by-default vs
off-by-default; how retries interact with `--max-model-calls` / `--turn-budget` so resume does not
become a hole in the spend ceiling; and that `usage`/`cost_usd` account for **all** attempts, not
just the successful one. A silent retry that inflates cost with no trace is an auditability
regression, and auditability is the property the whole runtime is built around.

**C-228 — capture wire evidence before theorising.** "Could not reproduce" is an acceptable outcome
*only* with the reproduction attempt described. If flux is at fault the fix carries a failing-first
test driven from a recorded envelope in the corpus. If OpenRouter/Google is at fault, the limitation
gets documented where a user *choosing a model* will see it, rather than discovering it mid-task.

## What done looks like

A long unattended run against a flaky provider either completes — having retried a transport failure
a bounded, visible, accounted number of times — or exits non-zero with a typed error a subprocess
driver can branch on without parsing prose. And `openrouter/google/gemini-3.x` is either usable for
sustained runs, or documented as not.

## Non-goals

- **No general-purpose retry framework.** Bounded resume for one named failure class, not a policy
  engine.
- **No change to how the agent handles task-level failures.** A refused model or a content-policy
  rejection remains the agent's problem to surface, not the runtime's to paper over.
- **No cross-provider transport abstraction.** Classify at the existing provider seam; do not invent
  a layer.
