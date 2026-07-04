# The agent loop

flux's turn loop **is itself a Flux-Lang program**. When you run `flux run "…"` (or type into the
REPL), the engine doesn't run a hardcoded Rust loop — it executes
[`crates/flux-flow/assets/agent-loop.flux`](../crates/flux-flow/assets/agent-loop.flux), and the Rust
side (`FlowEngine::run_turn_cancellable`) is just a thin bootstrap. This is the thesis — *the LLM is
not the runtime* — taken all the way down: even the loop that orchestrates the model's steps is a
readable plan, run through the same safety envelope as everything else.

The loop has three passes (design [`multipass-agent-loop.md`](designs/multipass-agent-loop.md)):

```
flow agent-loop -> string
  $answer = fmt("")
  $feedback = fmt("")
  $done = fmt("")

  # Pass 1 -- orient: one planner call, a three-way contract -- trivial request -> prose chat;
  # simple/actionable request -> the full execution plan; complex/context-hungry request -> a
  # small read-only gather plan + brief. $settled is "" only for the gather case.
  $plan = plan({ feedback: $feedback, phase: "orient" })
  $settled = $plan.settled

  # Pass 2 -- gather: bounded, read-only, approval-free rounds while not yet settled. Skipped
  # entirely when orient already settled, so a trivial/simple turn adds zero latency here.
  unless $settled
    repeat 3
      until $settled
      $ran = run_plan($plan)
      $feedback = $ran.transcript
      do observe "turn.gather", $ran
      $plan = plan({ feedback: $feedback, phase: "gather" })
      $settled = $plan.settled

  # Pass 3 -- plan / execute / revise: the standard loop, unchanged guards. A leftover gather
  # plan (the budget exhausted before settling) simply runs as the first execute iteration.
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
        # $ran.failure is a reified mid-plan halt (design Part 2) when this round's plan failed
        # part-way through — null (host-normalized, never a missing key) on a clean run. Routing on
        # it tells a revision round (the model is repairing a halt) apart from a plain iteration.
        $failure = $ran.failure
        when $failure
          do observe "turn.revision", $ran
        else
          do observe "turn.iteration", $ran
        $plan = plan({ feedback: $feedback, phase: "execute" })
  return $answer
```

`plan` re-enters the planner (the model compiles your request into a typed graph), `run_plan`
executes that graph **in the same session through the same approval + IO envelope**, and the
transcript is fed back as `$feedback` so the next `plan` sees what happened. The loop ends when the
model answers in prose.

**Orient is the turn's first `plan` call, not an extra round-trip** — a trivial or simple/actionable
request costs exactly as many provider calls as the old single-pass loop did; the gather pass never
runs its body when orient already settled. `phase` threads through the already-opaque `plan` input
(no protocol change), and `settled` is `""` only while an accepted `gather: true` plan is still being
worked (the model's own signal, enforced effect-clean and capped at ~12 call nodes at compile time —
never trusted blindly). The grounding `brief` a gather round carries is host-carried for the rest of
the turn — prepended to every subsequent `plan` feedback message, gather or execute — so a multi-round
gather never loses the thread. If the 3-round gather budget exhausts before settling, the leftover
plan just runs as the execute pass's first iteration — nothing is discarded.

A workspace `.flux/agent-loop.flux` override written before this phased loop shipped (a bare
`plan($feedback)`, no `phase`) keeps working unchanged: a phase-less `plan` call behaves as the
`execute` phase — byte-compatible with the pre-multipass contract.

**A mid-plan failure doesn't discard the plan (design Part 2, patch-and-continue).** When an
execute-phase plan fails part-way through, `run_plan` never errors the turn — it returns the
completed prefix's transcript plus a reified `failure` (`{node, stmt, op, kind, fatal, message,
completed[]}`) and the plan rendered with ✓/✗/· status markers. The loop observes `turn.revision`
instead of `turn.iteration` for that round (routing on `$ran.failure`) and feeds the structured
feedback to the next `plan` call. If the model re-emits a corrected plan that keeps the completed
statements byte-identical, the runtime fast-forwards past them (rehydrating their recorded values,
never re-dispatching them) and executes only the fixed suffix — the CLI/TUI render the halt in real
time (`✗ step 4/9 edit failed — revising…`) and the resumed plan's reused prefix marked `✓ (done)`.
A denied/refused statement is never silently re-dispatched unchanged; the model must choose a
different approach.

These reflexive ops — `plan`/`run_plan` plus the evidence ops
`observe`/`evidence`/`metrics`/`grade` — are documented in
[`crates/flux-flow/docs/ops-reference.md`](../crates/flux-flow/docs/ops-reference.md).

By design the loop is **invisible** during a normal turn: the machinery ops are filtered from the
surface so you see the real work (`read`/`edit`/`bash`/…), not the plumbing. The commands below let
you watch it, inspect what it recorded, and rewrite it.

## Watch it work live — `--show-loop`

```bash
flux run --show-loop "fix the failing test"
```

`--show-loop` (or `FLUX_SHOW_LOOP=1`) stops the surface from filtering the loop machinery, so each
iteration streams as it happens:

```
→ [1/25] plan       ask the model (phase: orient)
  ✓ {"kind":"plan","ast":{…},"complete":null,"settled":"true"}
→ [2/25] run plan   execute the emitted graph
    … the inner ops (read/edit/cargo_test) stream and gate here …
→ [4/25] observe    turn.iteration
→ [5/25] plan       ask the model (phase: execute)
  ✓ {"kind":"chat","text":"Fixed — the test passes now.","settled":"true"}
```

A complex/context-hungry request instead settles late: orient's gather plan runs a `turn.gather`
round (not `turn.iteration`) before the execute pass ever starts, and a `flow.brief` observation
marks the moment its grounding artifact was accepted.

A mid-plan failure adds one more shape to watch for: `run_plan` emits a `flow.halt` observation the
instant it halts (rendered as `✗ step 4/9 edit failed — revising…`), and the round's own `observe`
call fires `turn.revision` instead of `turn.iteration`:

```
→ [2/25] run plan   execute the emitted graph
    … completed steps stream normally, then the failing one …
  ✗ step 2/3 edit failed — revising…
→ [4/25] observe    turn.revision
→ [5/25] plan       ask the model (phase: execute)
  ✓ {"kind":"plan","ast":{…},"settled":"true"}
→ [6/25] run plan   execute the emitted graph   (statement 0 skipped — fast-forwarded)
→ [8/25] observe    turn.iteration
```

The machinery ops are pre-authorized engine control flow, so revealing them never adds approval
prompts. (`-v`/`--verbose` is separate — it un-caps tool *output*; combine them for the fullest view.)

## Trace the loop's structure — `--trace-loop`

`--show-loop` reveals *which ops* the loop dispatches; `--trace-loop` (or `FLUX_TRACE_LOOP=1`) goes
one level deeper and traces the loop program's own **structure** — one dim line per outer-loop round
and per structural AST node it executes (op calls with their bind name, which `when`/`unless`/`match`
branch was taken, `return`, and an until-guard exit) — while leaving the loop's normal output
completely unchanged when the flag is off:

```bash
flux run --trace-loop "fix the failing test"
```

```
⟳ round 1/25
· plan → $plan
· when $settled → else
⟳ round 4/25
· run_plan → $ran
· match $ran.failure = null → default
· return $answer
```

This only traces the **outer** agent loop (`agent-loop.flux`) — a plan's own internal ops still
stream via the normal tool-call output, never through this trace. The observations are live-only:
they never touch the value store, the run-event trail, or `/evidence`'s log.

## Inspect the evidence trail — `/evidence`

The loop and the dispatcher record an audit trail as the turn runs — tool calls, tool errors,
per-iteration markers, and any observation a flow emits. In the REPL:

```
/evidence
  evidence: 7 observations, 2 iterations, 1 error
    turn      tool_call        {"tool":"read"}
    turn      tool_error       {"tool":"cargo_test"}
    turn      turn.iteration   {"steps":3}
    …
```

This is the same shared log the `observe`/`evidence`/grading ops read, which is what makes the loop
*evidence-based*: it can branch on its own runtime observations. The trail is also **durable**: at
the end of every turn the engine flushes the log's new entries to the session's event store
(`events.db`) as `observation` events, and each planning attempt is recorded with the accepted
plan's fingerprint and its readable rendered graph — so a session's evidence survives process exit
and can be read offline (`flux_events::projection::observations`).

## Read & customize the loop — `flux loop`

```bash
flux loop show     # print the active loop (built-in, or a workspace override) + its source
flux loop eject    # write the built-in to .flux/agent-loop.flux so you can edit it
flux loop eject --force   # overwrite an existing override with the built-in
```

A workspace `.flux/agent-loop.flux` **overrides** the built-in loop — the engine parses and runs it
on the next turn (an invalid override is reported by `flux loop show` and fails the turn rather than
silently falling back). `eject` is just a convenience that drops the built-in text there for you to
edit; you can also write the file by hand. Because the loop is ordinary Flux-Lang, you can change the
iteration cap, add a `grade`-based stop condition, emit extra observations, or restructure the
control flow entirely — all within the same envelope.

## How it fits together

- **The loop is a plan**, not Rust — `assets/agent-loop.flux`, overridable per workspace.
- **The reflexive ops** (`plan`/`run_plan`) are tagged to a never-surfaced `reflect` group, so the
  model never sees them; only a pre-authored flow (the loop, or `flux flow run`) can call them.
- **Everything still dispatches through `Executor`** — the no-bypass safety envelope holds
  recursively, even for a plan that runs a plan.

See [architecture.md](architecture.md#agent-loop-sessions-context) for where this sits in the crate
layering, and [`ops-reference.md`](../crates/flux-flow/docs/ops-reference.md#agent-loop-ops-the-self-hosted-turn-loop)
for the op signatures.
