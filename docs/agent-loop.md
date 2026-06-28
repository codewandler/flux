# The agent loop

flux's turn loop **is itself a Flux-Lang program**. When you run `flux run "…"` (or type into the
REPL), the engine doesn't run a hardcoded Rust loop — it executes
[`crates/flux-flow/assets/agent-loop.flux`](../crates/flux-flow/assets/agent-loop.flux), and the Rust
side (`FlowEngine::run_turn_cancellable`) is just a thin bootstrap. This is the thesis — *the LLM is
not the runtime* — taken all the way down: even the loop that orchestrates the model's steps is a
readable plan, run through the same safety envelope as everything else.

The whole loop is ~20 lines:

```
flow agent-loop -> string
  $answer = fmt("")
  $feedback = fmt("")
  $done = fmt("")
  repeat 25
    until $done
    $plan = plan($feedback)          # ask the model for a plan (or a prose answer)
    $kind = $plan.kind
    match $kind
      case "chat"                    # the model answered in prose → the turn is done
        $answer = $plan.text
        $done = fmt("true")
      case "error"
        $answer = $plan.text
        $done = fmt("true")
      default                        # the model emitted a graph → run it, feed results back
        $ran = run_plan($plan)
        $feedback = $ran.transcript
        do observe "turn.iteration", $ran
  return $answer
```

`plan` re-enters the planner (the model compiles your request into a typed graph), `run_plan`
executes that graph **in the same session through the same approval + IO envelope**, and the
transcript is fed back as `$feedback` so the next `plan` sees what happened. The loop ends when the
model answers in prose. These reflexive ops — `plan`/`run_plan` plus the evidence ops
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
→ [1/25] plan       ask the model
  ✓ {"kind":"plan","ast":{…},"complete":false}
→ [2/25] run plan   execute the emitted graph
    … the inner ops (read/edit/cargo_test) stream and gate here …
→ [4/25] observe    turn.iteration
→ [5/25] plan       ask the model
  ✓ {"kind":"chat","text":"Fixed — the test passes now."}
```

The machinery ops are pre-authorized engine control flow, so revealing them never adds approval
prompts. (`-v`/`--verbose` is separate — it un-caps tool *output*; combine them for the fullest view.)

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
*evidence-based*: it can branch on its own runtime observations. (The log is per-session and held in
memory; it is not yet persisted across runs.)

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
