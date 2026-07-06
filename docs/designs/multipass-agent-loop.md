# Design: multi-pass agent loop — orient → gather → plan/execute/revise, with patch-and-continue

**Status:** MVP implemented (2026-07-02 — A-12–A-17 + L-22 shipped, full gate green; I-03
measurement pending) · **Pillar:** Agent (loop shape, UX) + Language (runtime ledger) ·
**Stories:** [A-12](../stories/A-12-unsilence-planning-wait.md) ·
[A-13](../stories/A-13-phase-aware-planner-protocol.md) ·
[A-14](../stories/A-14-multipass-agent-loop.md) ·
[A-15](../stories/A-15-phase-aware-surface.md) ·
[L-22](../stories/L-22-reified-halts-statement-ledger.md) ·
[A-16](../stories/A-16-loop-host-resume-policy.md) ·
[A-17](../stories/A-17-revise-wiring.md) ·
[I-03](../stories/I-03-multipass-cutover-measurement.md) (MVP) —
[A-18](../stories/A-18-multipass-plan-mode.md) ·
[L-23](../stories/L-23-streaming-plan-render.md) ·
[L-24](../stories/L-24-reified-await-ledger.md) ·
[L-25](../stories/L-25-flow-run-resumable-mode.md) (later)

## Why

A flux turn today one-shots a plan per loop iteration. Two costs:

1. **Perceived latency.** During the planning call in a *normal* turn the CLI shows nothing at all:
   `sink.planning(true)` is only invoked from `plan_turn` (the REPL `/plan` path,
   `crates/flux-flow/src/engine.rs:358`), and `EngineLoopHost::plan` passes `thinking_sink: None`
   into `compile_turn` (`crates/flux-flow/src/loop_host.rs:542-553`). The streaming plumbing exists
   end-to-end (`SharedSink` → `SinkEvent::Thinking/Planning` → `CliSink`) and is dead in the one
   mode users live in.
2. **Real latency.** The planner is instructed to "Put the WHOLE task in one plan"
   (`crates/flux-flow/src/compile.rs:951`) without having read anything, so plans are large (slow to
   emit), guessed (often wrong mid-way), and a mid-plan failure **discards the whole plan**: the
   error becomes a `[plan error] {e}` transcript (`loop_host.rs:742-757`) and the next iteration
   re-plans from scratch. Completed steps' symbol binds survive (FlowStore → SessionView → 3-arg
   `analyze_flow` definedness), but the plan itself and the runtime's knowledge of *what already
   ran* are thrown away.

Evidence this bites in practice: a terminal-bench smoke (2026-07-02) functionally solved
`fibonacci-server` (server up, every curl check correct) yet burned the 30-plan-iteration cap stuck
on one step — 480s, $0.58, scored 0. The `s_251` postmortem
([session-s251-postmortem.md](../archive/designs/session-s251-postmortem.md)) records the same shape: re-plan →
re-gather → re-starve, seven iterations, cancelled.

What the user asked for, verbatim intent: (1) orient/ground the query and figure out what context is
needed, (2) collect that context, (3) a plan loop that can update the plan, partially execute it,
fail in the middle, adjust, and **continue execution from that point**.

## Approach

Two coupled parts. Part 1 restructures the loop into visible passes and fixes the feedback vacuum.
Part 2 gives the runtime a memory of what already ran, so a corrected plan continues instead of
restarting. Either ships value alone; together they close the loop.

### Part 1 — the phased loop: phases live in the loop program, not in Rust

The turn loop stays a flux-lang program (the "one engine" decision, [flux-flow.md](flux-flow.md)
§11, holds — no Rust free-form loop returns). `crates/flux-flow/assets/agent-loop.flux` becomes:

```flux
flow agent-loop -> string
  $answer = fmt("")
  $feedback = fmt("")
  $done = fmt("")

  # Pass 1 — orient: one planner call. May answer (chat), emit the full
  # execution plan, or emit a small read-only gather plan + brief.
  $plan = plan(feedback: $feedback, phase: "orient")
  $settled = $plan.settled            # "" only while the model is still gathering

  # Pass 2 — collect: bounded, read-only, approval-free gather rounds.
  repeat 3
    until $settled
    $ran = run_plan($plan)
    $feedback = $ran.transcript
    do observe "turn.gather", $ran
    $plan = plan(feedback: $feedback, phase: "gather")
    $settled = $plan.settled

  # Pass 3 — plan / execute / revise: the standard loop, unchanged guards.
  repeat 25
    until $done
    $kind = $plan.kind
    match $kind
      case "chat"
        $answer = $plan.text
        $done = fmt("true")
      case "error"
        $answer = $plan.text
        $done = fmt("true")
      default
        $ran = run_plan($plan)
        $feedback = $ran.transcript
        do observe "turn.iteration", $ran
        $plan = plan(feedback: $feedback, phase: "execute")
  return $answer
```

**Normative semantics:**

- **Orient is the turn's first `plan()` call, not an extra round-trip.** Its contract is three-way:
  trivial → prose chat (as today, 1 call); simple/actionable → full execution plan (as today —
  "read a file then answer" stays exactly as fast); complex/context-hungry → a SMALL read-only
  gather plan tagged `gather: true` with a `brief: {goal, needs[]}` grounding artifact. The host
  computes `settled` ("" only for gather plans), so when orient answers or emits the full plan, the
  gather loop body never runs. Zero added latency for tasks that don't need it.
- **Protocol carrier:** the `plan` reflexive op gains a `phase` argument (threaded through the
  already-opaque JSON input — `LoopHost::plan` takes a `Value`, no trait change); `compile_turn`
  gains a phase parameter selecting a per-phase instruction segment. `emit_plan` (`EmitPlanInput`)
  gains optional `gather: bool` and `brief: {goal, needs[]}` fields, parsed like `complete`.
- **Prompt-cache discipline (A-03):** the per-phase contract is a *separate, byte-stable* cached
  segment appended after segment A; all phases share A's cache prefix. Segment C (symbols) stays
  last/uncached. The "WHOLE task in one plan" instruction is rescoped to *the execution plan*; only
  gathering is staged.
- **Gather is enforced, not trusted:** at compile, a `gather: true` plan must be effect-clean —
  every called op (composites included, via the registry's transitive effect metadata,
  `crates/flux-flow/src/registry.rs:211-307`) must be free of write/destructive effects, and the
  plan is capped at ~12 call nodes; violations are repair feedback exactly like hidden-op
  rejections. Once the gather budget (repeat 3) is spent, the `"execute"` phase contract rejects
  further `gather: true` emissions. `run_plan` re-checks `mutating` as defense-in-depth; a
  non-mutating plan already skips approval (`loop_host.rs:700`), so gather rounds never gate.
  Results land as ordinary FlowStore symbols + the feedback transcript; the brief is host-carried
  per-turn (beside `pending_completion`, reset in `set_turn`) and prepended to every subsequent
  planner feedback message.
- **Graceful degradation:** if the budget exhausts while the model still emitted a gather plan, that
  read-only plan simply runs as the first execute-loop iteration. Honest framing: multi-pass
  *front-loads and bounds* gathering; the execute loop can still emit read plans (legitimate
  read→fix iteration), bounded by the existing stall/token/25-cap guards.
- **UX:** wire `planning(true/false)` + thinking-token streaming in `EngineLoopHost::plan` (the
  single biggest perceived-latency win, independent of everything else); emit a `loop.phase`
  observation at `plan()` entry so the CLI/TUI spinner reads "orienting… / planning… / revising…";
  render the brief the moment it's accepted (`flow.brief` observation → `◆ goal: …`); render gather
  plans as a compact one-liner and execution plans as the full tree + risk badge; on failure render
  `✗ step 4/9 — revising…`. Streaming plan-skeleton rendering is deferred until after
  [L-20](../stories/L-20-emission-ab-measured.md)'s emission-arm decision (text-arm win makes it
  nearly free; don't build it twice).
- **Clean cutover, no flags:** the new `agent-loop.flux` IS the loop. A phase-less `plan($feedback)`
  call (existing ejected/overridden loops) gets `phase: "execute"` semantics — byte-compatible with
  today's contract. `flux loop eject` emits the new text. Plan mode (`flux plan`, REPL `/plan`) is
  unchanged in MVP (A-18 brings gather to it later). `PlanAttempt` gains a `phase` field so C-15
  efficiency metrics can report gather/revise rounds per turn.

### Part 2 — patch-and-continue: reified halts + content-addressed statement ledger

The mechanism in one paragraph: when a loop plan fails at top-level statement *i*, the runtime — in
a new **resumable mode** used only by `run_plan` (composites and nested bodies keep strict `Err`
propagation and structural fatality, F14) — does **not** return `Err`. It appends a `PlanHalted`
run-event (plan content key, failing index, statement content hash, classified failure kind) and
returns `Ok(FlowOutcome { failure: Some(PlanHalt{..}), transcript: <prefix outputs> })`. Every
completed top-level statement was already ledgered as `StatementCompleted { plan, node,
stmt_hash16, value }` as it ran. The model receives structured feedback (the plan rendered with
✓/✗/· markers plus a machine-readable `failure` object) and **re-emits the full corrected plan
through the unchanged `emit_plan` gates** — C-17 stays fully intact (one plan per turn, hidden-op,
analyzer, lower). The next `run_plan` folds the event log for the open halt latch (last `PlanHalted`
with no later `PlanResumed` — the `once_lookup` fold pattern), pairwise-compares the new plan's
statement hashes against the ledger from index 0, fast-forwards the longest matching completed
prefix (rehydrating each skipped statement's recorded value — strictly better than `checkpoint`,
which rehydrates nothing), appends `PlanResumed { skipped }` (one-shot latch consumption), and
executes from the first divergence.

**Normative semantics:**

- **Types (flux-lang):** `FailureKind { Denied, ConfirmDenied, AssertFailed, Runtime }` with
  `is_fatal()` mirroring `FlowError::is_fatal`; `PlanHalt { node: NodeId, stmt: String /*hash16*/,
  op: Option<String>, kind, message, plan }`; `FlowOutcome.failure: Option<PlanHalt>`;
  `ResumeLedger { completed: Vec<LedgerEntry{node, stmt, value: Option<ValueId>}>, prior_plan }`;
  new entry point `execute_flow_resumable(...)` beside the unchanged `execute_flow`.
  `FlowError` itself is **unchanged** — classification reads existing variants (L-21's
  `FlowError::Denied` shipped, commit `b84204d`, and is load-bearing here).
- **Statement identity:** `stmt_hash16 = sha256(canonical JSON of the statement)[..16]` — serde_json
  maps serialize key-sorted, so the hash is formatting- and key-order-insensitive across
  re-emissions. Plan identity reuses `flow_key` (`runtime.rs:76-87`).
- **RunEvent variants (additive):** `StatementCompleted { plan, node, stmt, value, skipped }`,
  `PlanHalted { plan, node, stmt, op, kind, error }`, `PlanResumed { plan, prior, skipped }`.
  Append-only philosophy preserved: "consumed" is expressed by appending `PlanResumed`, exactly as
  `once`/`checkpoint` state is a fold (`crates/flux-flow/src/state.rs:83-149`). No new SQLite
  table: `FlowStore::open_halted_plan(session)` is a fold over events.db — crash-tolerant and
  cross-process by construction. A crash mid-plan (no `PlanHalted` written) leaves no latch → next
  run starts fresh (conservative).
- **`run_plan` order of operations (flux-flow):** (1) parse + fingerprint (unchanged; silent-success
  guard unchanged — it only refuses byte-identical plans after a *successful* run, so a
  byte-identical re-emission after a halt is precisely "retry only the failed statement");
  (2) fold the latch; (3) **denial re-emission guard**: if the halt kind is `denied`/
  `confirm_denied` and the new plan contains a statement with the same `stmt_hash`, return an
  informational transcript *without executing* — policy/user said no; never silently patched
  around; a *different* approach flows normally; (4) compute the prospective skip prefix in-host,
  scope `plan_risk_with_composites` to the *suffix that will actually run* (the user isn't
  re-prompted to approve already-completed writes) and annotate the `flow.plan` observation with
  ✓-done/•-to-run markers; (5) execute resumable with the ledger; (6) on `outcome.failure`, update
  `LoopGuard` with a structured key `halt:{op}:{stmt}:{kind}` (same statement failing the same way
  twice escalates at existing thresholds) and build the feedback contract below.
- **Feedback contract:** transcript = prefix step outputs + `[plan halted at step N of M] …` +
  the plan rendered with ✓/✗/· markers + kind-specific guidance ("keep steps 0–3 byte-identical —
  the runtime will skip them; it re-runs any step you change"), placed before the
  `cap_loop_feedback` tail; plus machine-readable
  `failure { node, stmt, op, kind, fatal, message, plan, completed[{node, stmt, bind, op}] }` so a
  flux-lang loop can route on `$ran.failure.kind` / `$ran.failure.fatal`.
- **Failure-kind policy:** `runtime` → patch-and-continue (prefix skips, failed statement
  retries/edits); `assert_failed` → fatal, prefix skip allowed, feedback demands re-planning the
  remainder (the plan's own invariant broke); `denied`/`confirm_denied` → fatal + the re-emission
  guard above. If the re-emitted plan diverges *before* the failed index, the transcript warns
  "steps X–Y changed and will RE-RUN (including their side effects)".
- **Cross-turn/cross-process:** on a fresh turn, if `open_halted_plan` returns `Some`, `plan()`
  injects one ephemeral `[resume context]` message describing the halt and the completed prefix.
  No new suspension row; no take-before-run hazard (the await path's "post-await failure is
  unretryable" limitation is *not* inherited).
- **Primitive interplay:** `once` unchanged and complementary (the explicit, edit-surviving
  idempotency key — skipped statements never reach it; the feedback recommends it when the model
  wants to edit an executed effectful statement). `saga` composes correctly: a failing saga
  statement compensates internally then propagates, so it never ledgers as completed and re-runs
  wholly (compensation restored the pre-statement world). `checkpoint` coexists (ledger
  fast-forward runs after the checkpoint cursor, max of both); it remains the authored-named-flow
  primitive per [flux-lang-evolution.md](flux-lang-evolution.md) §5.1's deliberate scoping.
  Granularity: a failed top-level `each`/`repeat`/`parallel` re-runs wholly (consistent with
  checkpoint/await being top-level-only); finer granularity is out of scope.
- **Audit:** the log now answers, per statement, *executed / skipped / failed — and if skipped,
  which run produced the value* (`PlanAccepted` → `Step*` → `StatementCompleted{skipped:false}` →
  `PlanHalted` → next run `PlanResumed` + `StatementCompleted{skipped:true}` → `FlowReturned`).
  Statement events use NodeId positions + content hashes, sidestepping the known `StepId`
  input-hash collision.

### Change map (both parts)

| Area | Files |
|---|---|
| Loop text | `crates/flux-flow/assets/agent-loop.flux` |
| Planner protocol + gates | `crates/flux-flow/src/compile.rs` (`EmitPlanInput{gather,brief}`, phase segments, gather validation) |
| Loop host | `crates/flux-flow/src/loop_host.rs` (phase threading, brief carry, sinks, latch fold consumption, denial guard, suffix-scoped approval, feedback contract, guard keys) |
| Runtime core | `crates/flux-lang/src/runtime.rs` (resumable mode, ledger fast-forward, halt reification, `stmt_hash16`), `crates/flux-lang/src/ast.rs` (RunEvent variants), `crates/flux-lang/src/render.rs` (`render_statement` for markers) |
| Store | `crates/flux-flow/src/state.rs` (`open_halted_plan` fold), `crates/flux-flow/src/runtime.rs` (resumable-with-composites wrapper) |
| Ops spec | `crates/flux-tools/src/reflect.rs` (`plan.phase`, `run_plan` outcome doc) |
| Surface | `crates/flux-cli/src/main.rs` (`CliSink`), `crates/flux-tui` (parity) |
| Events | `crates/flux-events` (`PlanAttempt.phase`) |
| Docs | `docs/agent-loop.md`, `docs/usage.md`, `crates/flux-flow/docs/ops-reference.md`, [flux-flow.md](flux-flow.md) §5.2 note |

## Alternatives considered

- **Multi-pass inside `compile_turn` (Rust).** Rejected: `compile_turn` executes nothing today; a
  gather pass inside it would need execution inside the compiler — a bypass seam invisible to
  `--show-loop`, un-overridable, and a de-facto second Rust loop (violates flux-flow.md §11).
- **A dedicated `orient()` reflexive op.** Rejected: a dedicated op means a dedicated provider
  round-trip *before* the first plan — the "read a file then answer" case gets strictly slower.
  Orient is a contract of planner call #1, so it's an argument, not an op.
- **Implicit per-statement `once` labels for resume.** Rejected: positional labels shift on
  insertion → the runtime skips *new* work believing it already ran (silent wrong-execution, the
  worst available failure mode); content-derived labels are just prefix hashing with the
  bookkeeping pushed onto the model; `once` records aren't one-shot, so later legitimate "do it
  again" turns would silently skip.
- **`patch_plan(token, edits)` DSL.** Deferred, not adopted: it structurally enforces
  "only the suffix changes" and saves emission tokens, but adds a second emission channel beside
  `emit_plan` (eroding C-17's one-plan-per-turn), models are measurably worse at edit scripts than
  re-emission (`edit` op `old_string` misses are already a tracked deterministic-failure class),
  and patch application must reconstruct + re-validate the full plan anyway. Re-emission costs
  nothing new — the loop already re-emits a full plan every round; the win is skipped *execution*.
  If plan sizes ever make emission cost bite, `patch_plan` layers on top (patch → full plan → same
  gates → same ledger).
- **Reusing the await `suspensions` table for halts.** Rejected: trigger semantics differ (await
  consumes the next user message pre-planning; a halt is consumed by the next `run_plan`), and
  sharing the one-row latch would let a mid-plan failure clobber a pending await. The *pattern*
  (fold-derived latch) is reused; the table is not.

## Risks & open questions

1. **Gather over-use** — an extra provider round on tasks the model could one-shot. Mitigated by
   the orient contract, the repeat-3 cap, and measured by I-03 before the cutover is called done.
2. **Prompt regression** — rescoping "WHOLE task in one plan" risks tiny-plan dribble. Keep it
   verbatim for the execution plan; only gathering is staged. I-03 watches plans-per-turn.
3. **Prompt-cache erosion** (A-03's hit rate) if phase segments are misplaced — they must be
   separate, byte-stable, after segment A. I-03 verifies hit rates.
4. **Effectful re-runs on prefix edits** remain possible by design (the conservative direction);
   fenced by suffix-scoped approval, per-op dispatch gates, the RE-RUN warning, and `once`. Open:
   should a High-risk re-run escalate to a confirm? Decide in A-16 review.
5. **Stale-latch skips** — a next-turn plan coincidentally opening with a hash-identical statement
   skips it with the recorded value. Bounded by one-shot consumption, prefix contiguity, value
   rehydration, and the disclosed `[resume context]`. Add a latch TTL only if it bites.
6. **Event-schema forward compatibility** — additive RunEvent variants follow the P7 precedent,
   but an older binary reading a newer log errors; tolerant-read hardening is a recorded residual.
7. **Denial-guard strictness** — refusing hash-identical refused statements could block a
   legitimately re-approved retry after a policy change; current answer: change the statement or
   ask the user. Revisit with policy-surface work.
8. **Spirals are bounded, not abolished** — the execute loop can still read repeatedly; the
   stall/token/25-cap guards remain the backstop, as today.
   *Update (2026-07-03, [A-20](../stories/A-20-stall-guard-resource-aware.md)):* the backstop is
   now resource-aware. The byte-exact transcript stall was defeated in practice by renamed-symbol
   re-reads (the `s_346` runaway: 22 read-only rounds, no answer); a per-turn `ReadTracker` at the
   dispatch seam now keys every read on `op + resolved args` (rename/reorder-insensitive by
   construction), escalates after 2 consecutive no-new-evidence rounds and force-stops honestly at
   3, and serves exact-repeat filesystem reads from a write-invalidated cache with an
   `already read as $X — reusing` note.
   *Update (2026-07-03, [A-28](../stories/A-28-read-coverage-stall-guard.md)):* freshness for
   `read`-shaped dispatches (a `path` plus optional `offset`/`limit`, no other params) is now
   **coverage-based**: a per-path covered-line interval set decides whether a window contributed
   unread lines, so sliding the window over an already-covered file (the `s_355` runaway: 25
   rounds, one file, offsets 2180→2990) stalls exactly like a renamed re-read, while a first pass
   paging through new regions never trips the guard. The escalate/stop feedback names the covered
   files and line spans; ops with other semantic params (grep/glob) keep exact `op+args`
   freshness, and write-invalidation clears coverage along with the cache.
   *Update (2026-07-04, [A-29](../stories/A-29-readonly-round-budget.md)):* the claim is now true
   for the **breadth** case too. Every guard above detects *redundancy*; a novelty treadmill (the
   `s_356` runaway: 22 rounds, a NEW grep pattern or a fresh window over new lines every round) is
   genuinely fresh to all of them, so the only remaining exit was the model choosing prose. A
   freshness-INDEPENDENT ladder now counts **consecutive read-only rounds** (any read, no effectful
   dispatch — an effect resets it, a no-read round leaves it unchanged): the "answer now from the
   session symbols" escalation at 6, the honest stop at 10 (`READONLY_ROUNDS_ESCALATE/STOP`), both
   carrying the evidence inventory (rounds, distinct resources, A-28 coverage spans). Legitimate
   read-heavy work raises the ceiling via `[limits] readonly_rounds_escalate/_stop` in
   `.flux/config.toml` (0 disables a rung) rather than defeating the detector.
   **Recorded decision — per-turn token budget stays default-OFF** (the A-29 acceptance asked for
   an explicit call now that A-26 measures cumulative billed tokens): the pathological read case is
   now bounded in *rounds*, which is model- and pricing-independent; a default token ceiling would
   instead cut off legitimately long turns (multi-file refactors, big-context models) at a
   threshold no single number gets right across providers. `--turn-budget` / `FLUX_TURN_TOKEN_BUDGET`
   / `[limits] turn_token_budget` remain the opt-in hard ceiling for cost-sensitive hosts.

**Residuals (recorded up front):** `patch_plan` token optimization; orphaned-ledger crash recovery
(recovering `StatementCompleted` trails with no `PlanHalted`); High-risk-rerun confirm escalation;
tolerant run-event reads; per-iteration ledger granularity inside `each`/`repeat`.

## Acceptance / done

- The phased loop ships as the one `agent-loop.flux` (no flags, old ejected loops byte-compatible);
  `--show-loop` shows the passes; trivial/simple turns make exactly as many provider calls as today.
- Normal-mode planning is never silent: planning state + thinking stream; phases label the spinner;
  the brief renders on acceptance.
- A mid-plan failure yields a reified halt with the prefix transcript; a corrected re-emission
  fast-forwards the matching prefix (values rehydrated), re-passes every C-17 gate, and the event
  log tells the true executed/skipped/failed story per statement.
- Denied statements are never re-dispatched unchanged; fatality classes hold
  (`policy_denied_statement_is_not_reattempted_via_resume`).
- Full dev-gate green; I-03 reports time-to-first-feedback, gather/revise rounds, tokens/turn, and
  terminal-bench pass-rate against the pre-cutover baseline — the cutover is judged on that
  evidence, not vibes.

## I-03 measurement results (2026-07-05)

Setup: baseline = pre-cutover main `b528772` (parent of cutover `e3ba495`), post = current main
(v0.2.17 release commit — the cutover plus the read-loop follow-ups A-20/A-28/A-29 that shipped on
top of it). Model for every run: `openrouter-anthropic/anthropic/claude-sonnet-4.6`. Harnesses:
`bench/run-ttff.sh` / `bench/run-tbench-compare.sh` (raw recordings + both legs' full reports kept
under `bench/*/results/i03-go/`).

**Time-to-first-feedback** (spawn → first rendered artifact, median over 3 trials per leg,
5-prompt fixed corpus, 30/30 runs clean — `failed_trials: 0` in every cell):

| prompt | baseline | post | delta |
|---|---|---|---|
| chat-trivial | 3615.0ms | 3189.8ms | −425.2ms |
| read-one-file | 4180.5ms | 2344.2ms | −1836.3ms |
| grep-count | 6982.0ms | 2342.7ms | −4639.3ms |
| write-summary | 8168.8ms | 3363.9ms | −4804.9ms |
| explore-complex | 7569.5ms | 2457.4ms | −5112.1ms |

Post wins every prompt. The win scales with task complexity (−0.4s trivial chat → −5.1s
multi-file exploration): the phased loop starts rendering gather-phase activity while the
pre-cutover loop was still silently composing one big plan. `planning_ms` (first planning-state
indicator) is ~71ms on post and null on baseline — the A-12 "silent while planning" bug is
measurably gone.

**Rounds / tokens per turn** (C-15 efficiency projections over the same 30 trial event stores,
`flux usage` per isolated trial HOME; median of 3 trials, spread noted where trials diverged):

| prompt | calls/turn b→p | plans/turn b→p | gather/turn (post) | revise/turn (post) | uncached-in b→p |
|---|---|---|---|---|---|
| chat-trivial | 1 → 1 | 0 → 0 | 0.0 | 0.0 | 19.9k → 20.5k |
| read-one-file | 2 → 2 | 1 → 1 | 0.0 | 0.0 | 20.1k → 20.7k |
| grep-count | 2 → 2 | 1 → 1 | 1.0 | 0.0 | 20.1k → 41.1k |
| write-summary | 2 → 2 (one trial 3) | 1 → 1 (one trial 2) | 0.0 (one trial 1) | 0.0 | 21.0k → 22.2k |
| explore-complex | 3 → 3 (one trial 2) | 2 → 2 (one trial 1) | 2.0 (one trial 1) | 0.0 | 61.2k → 62.8k |

Verdicts, honestly reported:
- **No tiny-plan dribble**: plans/turn is identical or lower on post for every prompt.
- **No call inflation**: trivial/simple turns make exactly as many provider calls as baseline (the
  design's acceptance bar); complex turns are the same or one fewer.
- **Revise rounds: 0.0 everywhere** — no revise-round churn on this corpus.
- **Token regression on gather-shaped prompts**: when post spends a gather round it re-pays the
  ~20k prompt prefix uncached — grep-count doubles uncached-in (20.1k → 41.1k). Total corpus spend:
  baseline $1.35 vs post $1.82 (+35%). This is the A-03 erosion watch firing: `cache-read` reports
  0% on BOTH legs on the `openrouter-anthropic` wire, so the gather round's prefix re-read is billed
  fully uncached here. Leg-neutral wire, honest caveat: on a cache-serving wire (direct Anthropic)
  the gather round's re-read should mostly hit cache and shrink this delta; not measured in this
  run. Follow-up candidate: verify prompt-cache headers ride the openrouter-anthropic codec.

**Terminal-bench pass-rate** (`terminal-bench-core==0.1.1`, tasks `chess-best-move` +
`fibonacci-server`, 3 trials/leg, same model; `bench/tbench-compare/results/i03-go/`):

- **baseline (valid)**: 0/2 tasks pass-all, mean checks 14%. `chess-best-move` 0% — all trials
  burned the 30-plan-iteration cap and one wrote the stop-notice into `move.txt` instead of a move;
  `fibonacci-server` 27.8% checks, 0 passes — Node was absent in the container and the loop
  improvised a Python server, failing the server-contract checks.
- **post (valid re-run after a key top-up; the first attempt 402'd at the first planner call and
  is kept as `post-report-402.txt`, excluded from scoring)**: 0/2 tasks pass-all, mean checks
  **0%** — a regression on partial credit. Failure signature, both tasks: **plan emission
  truncated at `max_tokens` (16384) before the plan finished**, retried repeatedly —
  `fibonacci-server` burned 31 steps / 22.9k output tokens / $0.76 per trial (baseline: $0.19)
  without ever standing up the server; `chess-best-move` lost its sub-agent turn to the same
  truncation and wrote a wrong move. Post is also slower to fail (mean wall 429s/269s vs baseline
  150s/111s).

**Verdict, honestly**: pass-all ties at 0/6 vs 0/6 — neither loop solves these two tasks with this
model — but baseline kept 14% partial checks where post kept 0%, and post pays more to fail. The
regression is not "the phased loop reasons worse": it's mechanical — execute-phase plans on
write-heavy tasks exceed the 16k emission ceiling, truncate, and the repair loop re-pays the
attempt. Filed as **A-40** (split/stream oversized plan emission; L-39's `"""` strings shrink the
representational bloat and the flux-planner corpus now records these truncations). Small-n caveat
throughout: 2 tasks × 3 trials, one vision-gated (chess) — directional evidence, not a pass-rate
claim.

## A-40: truncation split-repair (2026-07-06)

The mechanical failure above — a `max_tokens` cutoff drops the in-flight `emit_plan` tool_use block
(the provider never sends its `content_block_stop`), so `compile_turn` saw only a preamble (often
empty) and errored the whole turn — used to force the *caller* to re-plan from scratch: the same
oversized plan, re-emitted whole, at full price, forever (the fibonacci-server signature above).

The fix makes truncation its own bounded **repair class inside `compile_turn_inner`**, the same
shape as the existing hidden-op/diagnostics/decode-error repairs, instead of an immediate `Err`:

- A `max_tokens` stop gets up to `TRUNCATION_REPAIRS` (2) split-repair attempts before the turn
  fails. Each repair tells the model, arm-aware: emit a **smaller** complete plan (first few
  statements only, `complete` omitted so the phased loop's existing continuation contract — A-14 —
  calls `plan()` again for the rest), and hoist large literal payloads (e.g. one big file write) into
  their own follow-up plan. The text arm additionally points at the `"""` verbatim multi-line string
  spelling the grammar already teaches (L-39) — JSON-escaped `\n` payloads are exactly what inflates
  a text-arm emission; the JSON arm doesn't get that hint (`"""` has no meaning inside the `ast`
  payload's plain JSON strings).
- A further truncation past the repair budget still fails legibly, naming the ceiling and that the
  split repair was attempted — never a silent loop to step-budget exhaustion.
- Message-shape discipline (the session-alternation safety invariant) holds through the repair: if
  the truncated response had a non-empty preamble, the assistant message was already pushed, so the
  repair rides as a fresh user message; if the preamble was empty, nothing was pushed for that step,
  so the repair is appended onto the tail of the already-pending user message instead of adding a new
  one (which would be user-after-user). Both cases go through one helper so the two shapes can't
  drift apart.
- No new continuation machinery: the phased loop already re-plans after any plan without `complete`,
  so a split plan simply continues on the next round exactly as A-14 designed.

This turns "truncation kills the turn and forces a whole-plan retry" into "truncation shrinks the
plan and the loop keeps going" — bounded, so a plan that structurally cannot fit under any split
(one irreducible giant literal) still fails fast instead of spinning. (The repair mechanism and its
message-shape/arm-gating tests are covered by `crates/flux-flow/src/compile.rs`'s A-40 test group.)

**Live re-run verdict (2026-07-06, fibonacci-server × 3 trials, same model/dataset/ceiling, fixed
binary — `bench/tbench-compare/results/a40-fix/`):** the failure mode is **eliminated** —
`truncated at max_tokens` occurs 0 times across all trials (the repair is silent by design, so
signature absence is the success condition), and the sampled trial completes the write-heavy plan
first-shot at **4 steps / 73.3s / $0.3482** vs the I-03 post leg's 31 steps / $0.7553 per trial
that never completed. Honest residual: task checks stayed 0% for an independent, pre-existing
harness gap found during this validation — the tb container agent never enables the `shell` group,
so no leg (I-03 baseline included) could ever *start* the server it wrote → filed **I-04**; tbench
pass-rates before and after A-40 are equally depressed by it and remain comparable.
