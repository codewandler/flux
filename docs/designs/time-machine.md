# Design: Time Machine — hermetic replay, fork-at-any-decision, run-diff

**Status:** implemented 2026-07-07 (phases 0–3: C-43 · A-45 · A-46 · C-44; A-47 cockpit optional,
backlog) · **Pillar:** Agent/Core (cross-pillar) · **Stories:** C-43 · A-45 · A-46 · C-44 · A-47

> Implementation deltas vs this design (recorded in the stories' Progress): the fork boundary is a
> **scope swap** (Replay→Record at the divergence point) rather than a `boundary_seq` cursor — the
> live tail records its own cassette, making forks first-class replayable sessions; `--inject`
> executes a synthetic bind-plan (not a store-level bind) for the same reason; and the envelope's
> existing result-scrub (C-13) makes engine-path inputs redaction-stable end-to-end, so the
> dual-hash matcher is belt-and-braces rather than load-bearing there.

## Why

Every mainstream agent framework lets the LLM *be* the control flow, so its runs are irreproducible
by construction. flux's founding thesis is the opposite — **the LLM is not the runtime**: the model
compiles each turn into a typed Flux-Lang plan and a deterministic runtime executes it. That makes a
flux run a **deterministic artifact**, and the architecture has been quietly accumulating everything
needed to travel through one:

- The accepted plan of every turn is already persisted as canonical, re-parseable Flux-Lang text
  (`PlanAttempted.plan_source`, invariant `parse(format(ast))==ast`, `crates/flux-flow/src/loop_host.rs:1156`),
  redacted — so **replay needs no model call**.
- The execution core is deterministic: value ids are rowid insertion-order (`crates/flux-flow/src/state.rs:249`),
  symbols are last-writer-wins, `FlowClient::parse` is model-free (`crates/flux-sdk/src/flow.rs:315`),
  stores are run-isolated (`execute_with`, `flow.rs:404`), `now_ms` is metadata-only, no RNG/UUID in the loop.
- `RunEvent` (`crates/flux-lang/src/ast.rs:964`) is *literally documented* as "the replayable record"; its
  `run_trace()` projection (`crates/flux-events/src/store.rs:749`) is fully persisted and has **zero consumers today**.
- A fork-at-node engine already exists: `run_top_level_resumable` (`crates/flux-flow/src/runtime.rs:1108`)
  matches a plan's longest content-hash prefix against a `ResumeLedger`, rehydrates skipped statements,
  and re-runs from the first divergence; `resume_flow_named` (`runtime.rs:918`) injects a value at a node.

**The one missing piece.** Values live in an *ephemeral* in-memory SQLite store; only event
*references* persist (`FlowStore.conn` vs the durable `EventStore`, `state.rs:161`). So
`RunEvent::StepSucceeded{output: ValueId}` records a pointer into a store that dies with the process —
**op output DATA is never durable.** The design docs postponed exactly this ("audit-replay
postponed; re-run against live data is v1", `docs/designs/flux-flow.md:284`). Closing that gap — a
redacted **op-output cassette** — turns "re-run against today's world" into "faithfully reproduce that
exact past run, offline, zero model calls, zero side effects."

This is the capstone the whole architecture was built toward, and it advances all three pillars at
once: the **Language** (the plan is the replayable artifact), the **Agent** (debug/branch/steer any
run), and the **Improvement** loop (deterministic regression + counterfactual eval — "what if the
model had chosen differently").

## Approach

Three verbs over any recorded run:
- `flux replay <run>` — re-execute a past run exactly, offline, model-free, side-effects served from tape.
- `flux fork <run> --at <node>` — branch a run at any decision node and explore a different path.
- `flux diff <runA> <runB>` — align two runs and show where the plan, or the world, diverged.

**Grounding (verified against the tree).** (1) Values ephemeral, references durable → the cassette is
the one genuinely new capture. (2) One universal dispatch chokepoint: both interpreter levels bottom
out in `flux-lang`'s `execute_call → executor.dispatch(op,input)` (`runtime.rs:363`), but the
`Redactor` lives at L3 (`executor.context().redactor`), so capture must be an **L3 OpHost decorator**,
not a change inside L0 `execute_call`. (3) `EventKind::Run(RunEvent)` (`crates/flux-events/src/kind.rs:55,232`)
→ a new `RunEvent` variant rides the existing envelope: no new `EventKind` arm, no new table, one
projection path.

**The cassette (the one new capture).** Add `RunEvent::OpRecorded { seq, step, op, input_hash,
input_hash_redacted, content, view, is_error, denied, redacted, truncated }` to `ast.rs` (L0), every
new field `#[serde(default)]` so existing on-disk rows still decode. Keyed `(session, seq)` for
replay order + `(op, input_hash)` for integrity/divergence — with two review-mandated refinements:
(1) **redaction-aware matching** — the live run hashes/binds *unredacted* data (`runtime.rs:348`)
while the cassette serves *redacted* content, so a replayed input downstream of a redacted output
hashes differently; cells therefore carry `redacted: bool` + `input_hash_redacted =
sha256(redact(input))`, and the replay guard accepts a match on either hash (sound because
`Redactor::redact` is deterministic longest-first containment replacement, so redaction commutes
with interpolation). (2) **out-of-order-tolerant matching** — `parallel` branches dispatch
concurrently (`try_join_all`, `runtime.rs:1477`), so record-time interleaving is nondeterministic;
the replay matcher scans forward from the cursor for the first *unconsumed* cell matching
`(op, hash)` instead of demanding the strict next cell (sequential runs degenerate to the strict
cursor; divergence = no matching unconsumed cell). Captured by a new `CassetteHost` OpHost decorator
(`crates/flux-flow/src/cassette.rs`, L3) — `Off | Record | Replay` — that self-installs from the
executor context (like `set_session`, `engine.rs:229`) at every `ExecutorHost` construction (inner
`loop_host.rs:1501` for leaf ops; outer `engine.rs:244` only as fallback for >32K plans whose
`plan_source` was dropped). Record: dispatch → redact → append `OpRecorded` → return the unredacted
outcome to the live run. Replay: serve the next cell **without touching the inner host** (side
effects never fire); a mismatched `op`/`input_hash` is a hard `ReplayDiverged{at}` error, never
silent. Reuse the **exact** redactor that already scrubs `plan_source`/observations (C-13/C-22).
On by default (`--no-cassette` / `FLUX_CASSETTE=0` to opt out); per-op cap (`FLUX_CASSETTE_MAX_BYTES`,
default 1 MiB) keeps the head with `truncated=true`.

**Session-fork primitive.** `FlowStore::fork_session(events, src, at) -> ForkHandle`
(`crates/flux-flow/src/fork.rs`, L3): rather than copy raw (unredacted) value rows, **replay the
prefix hermetically** into a fresh session (`correlation_id = src`, reusing the sub-agent linkage
`crates/flux-orchestrate/src/lib.rs:333`) up to `boundary_seq` (first `OpRecorded` after
`StatementCompleted{node=at-1}`), rebuilding symbols + values for free.

**Surfaces.** `flux replay` (non-agent subcommand): read `run_trace`+`turns`, and execute each
accepted plan **in recorded order, reproducing the loop host's recorded dispositions** — an accepted
plan followed by no statements/cells was the A-05 identical-plan *skip* and must not be re-executed;
halted plans replay only their completed prefix — parsing each `plan_source` and executing under
`CassetteHost(Replay)` over a fresh store + recorded `ResumeLedger`,
render via the existing `CliSink`/`style_marked_plan`/`format_evidence`. Offline because the lazy
provider (`main.rs:2289`) never constructs a client (no model op reached). Sub-agent children replay
recursively via a new `EventStore::children_of(session)` projection. `flux fork` (agent-path
subcommand): clone-at-node, then diverge — **A** inject a value (`resume_flow_named`), **B** re-plan
the tail live (real `plan` op, nondeterministic by design), **C** edit the plan text
(`run_top_level_resumable`). The **cassette-vs-live boundary is `boundary_seq`**: prefix served from
tape (no side effects), tail through the **real** `Executor::dispatch` envelope (approval unchanged).
`flux diff` (non-agent): pure `run_diff(a,b)` read-model in `crates/flux-events/src/projection.rs`,
aligning statements by node and classifying via `stmt_hash16` (plan divergence) vs aligned
`OpRecorded.content` (output divergence).

**Layering** (all downhill; `flux-codegate` guards it): `OpRecorded` L0 · `set_cassette` on context L2 ·
`run_diff`/`children_of` L2 · `CassetteHost`/`fork_session`/replay L3 · subcommands L6 · cockpit L6.

Full detail — cassette schema, algorithms, keying, redaction posture, per-phase tests — lives in the
local implementation plan `.flux/plans/time-machine.md` (gitignored, per doc conventions).

## Alternatives considered

- **Live re-execution (no cassette).** Re-run the recorded plan against today's world; ships in days
  but is not a faithful reproduction — reads see today's files and mutating ops re-fire. Rejected as
  the headline (kept as a degenerate mode): it undersells the one thing only flux can do — *exact*
  reproduction. Hermetic capture is what makes replay/fork/diff trustworthy.
- **Sidecar cassette table / new `EventKind` variant.** Rejected: a new `RunEvent` variant rides the
  existing `EventKind::Run` envelope with no new table, no 14-arm `EventKind` match churn, and lands
  in the same `run_trace()` projection all three verbs already read.
- **Copy raw value rows on fork.** Rejected: would put *unredacted* secrets on disk, defeating the
  ephemeral-values design. Hermetic prefix-replay reconstructs state from the redacted cassette instead.
- **Alternative mega-features weighed and set aside:** Plan Certificates (prove-before-run safety
  proofs) and a Live-Cockpit-only slice. Time Machine won on uniqueness (only flux can do it), latent
  readiness (~70% already in the tree), and three-pillar reach.

## Risks & open questions

- **Hidden nondeterminism breaks `replay==record`.** Mitigated: verified-safe sources (rowid ids, LWW
  symbols, metadata-only `now_ms`, model-free parse, no RNG); Replay *serves* every op so no live
  time/RNG/env leaks into content; any residual control-flow dependence changes `input_hash` and
  **trips the divergence guard** — surfaced as a hard error, never silent corruption.
- **Redaction gap leaks a secret to durable `events.db`** (the cassette holds file/bash/http bodies).
  Mitigated: reuse the *exact* redactor already gating `plan_source`/observations, redact `content`+`view`,
  keep the cap, `--no-cassette` for sensitive runs. Residual = the same accepted posture as today's
  `plan_source` storage.
- **Two-level interpreter + irreversible side effects.** The inner `ExecutorHost` is rebuilt inside
  `run_plan` (`loop_host.rs:1501`), so the decorator self-installs from the executor context at every
  construction; Replay never re-enters live IO. Fork's live tail acts on today's world past
  `boundary_seq` — intended, gated by the real approval envelope, documented as the explicit
  cassette-vs-live safety line.
- **Residuals of the review-mandated matcher refinements** (2026-07-07 review): with redaction-aware
  dual-hash matching, two dispatches whose inputs differ only inside redacted spans alias to the same
  `input_hash_redacted` — the out-of-order matcher could serve them swapped; rare (identical op +
  redaction-equal input) and bounded (both cells came from the same recorded run). With out-of-order
  matching, two genuinely identical `(op, input)` dispatches recording different outputs can swap on
  replay — same bound: both outputs are real recorded outputs of that exact call.
- **Open:** cassette size growth — default-on records every read/bash/http body (≤ cap) into
  `events.db` on every run; C-43 must measure the bloat on a typical coding session and pick the
  default (on at 1 MiB vs a smaller cap vs opt-in) from evidence, and a retention sweep (the C-18
  TTL precedent) is likely needed before this ships in a release. Also open: whether
  `flux fork --replan` should pin a seed for reproducibility once provider seeding lands.

## Acceptance / done

The union of the member stories' acceptance. End-to-end proof:
`flux run -m mock 'edit a file'` → `flux replay last` reproduces the transcript **with zero API
spend and no provider constructed** → `flux fork last --at <n> --replan` explores a different tail
through the real approval envelope → `flux diff <orig> <fork>` pinpoints the divergence. Per-capability
failing-first tests (hermetic determinism, cassette redaction, model-free replay, fork prefix+diverge,
fork-tail-keeps-envelope, diff-detects-divergence, divergence-surfaces-loudly) green, plus the full
gate in both workspaces.
