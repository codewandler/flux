---
title: The agent loop
description: "The turn loop design, including orient-gather-plan behavior and what it is doing at each stage."
---

# The agent loop

flux's turn loop is itself a Flux-Lang program. When you run `flux run "..."` or type into the REPL,
the engine does not execute a hardcoded Rust loop; it runs `agent-loop.flux` through the same runtime
as every other plan.

This is the thesis, *the LLM is not the runtime*, applied to the agent loop itself: the model can
propose plans, but the deterministic flow controls when those plans are accepted, executed, revised,
or stopped.

Each turn, the loop calls two reflexive operations. `plan` re-enters the planner, so the model
compiles your request into a typed [Flux-Lang](../language/overview.md) graph (or answers in prose).
`run_plan` executes that graph in the same session, through the same authorization, approval, and IO
checks described in [Concepts](../concepts.md). The resulting transcript is fed back so the next
`plan` call sees what happened. The loop ends when the model answers in prose.

## Three passes

The loop runs in three phases, and a turn only pays for the passes it needs.

1. **Orient** — a single planner call with a three-way contract. A trivial request gets a prose
   answer; a simple, actionable request gets the full execution plan; a complex, context-hungry
   request gets a small read-only *gather* plan plus a grounding brief.
2. **Gather** — bounded, read-only, approval-free rounds that run only when orient asked for
   context. It is skipped entirely when orient already settled, so a trivial or simple turn adds no
   latency here.
3. **Plan / execute / revise** — the standard loop: emit a plan, run it, feed back the transcript,
   repeat until the model answers.

Because orient *is* the turn's first `plan` call rather than an extra round-trip, **a trivial or
simple request costs exactly as many provider calls as a single-pass loop would** — the gather pass
never runs its body when orient settles. If the gather budget runs out before the request is
grounded, the leftover plan simply runs as the first execute iteration; nothing is discarded.

## Patch-and-continue on failure

A mid-plan failure doesn't throw the plan away. When an execute-phase plan fails part-way through,
`run_plan` returns the transcript of the steps that completed plus a structured description of the
halt (which node, which operation, whether it was fatal), and marks each step with a ✓/✗/·
status. The loop feeds that back and asks the model to revise.

If the model re-emits a corrected plan whose already-completed steps are byte-identical, the runtime
**fast-forwards** past them — reusing their recorded values instead of re-running them — and executes
only the fixed suffix. A denied or refused step is never silently re-run unchanged; the model must
choose a different approach.

## Watch it work — `flux run --show-loop`

By default the loop is invisible: its machinery is filtered from the surface so you see the real work
(`read`, `edit`, `bash`, …), not the plumbing. Reveal it when debugging:

```bash
flux run --show-loop "fix the failing test"
```

Each iteration then streams as it happens:

```text
→ [1/25] plan       ask the model (phase: orient)
  ✓ {"kind":"plan","settled":"true"}
→ [2/25] run plan   execute the emitted graph
    … the inner ops (read/edit/cargo_test) stream and gate here …
→ [4/25] observe    turn.iteration
→ [5/25] plan       ask the model (phase: execute)
  ✓ {"kind":"chat","text":"Fixed — the test passes now.","settled":"true"}
```

A mid-plan failure adds a revision round — a halt renders in real time
(`✗ step 2/3 edit failed — revising…`) and the resumed plan's reused prefix is marked done.
The machinery is pre-authorized engine control flow, so revealing it never adds approval prompts.
(`-v` is separate — it un-caps tool *output*.)

## Inspect the evidence — `/evidence`

The loop records an audit trail as the turn runs: tool calls, tool errors, per-iteration markers, and
any observation a flow emits. In the REPL:

```text
/evidence
  evidence: 7 observations, 2 iterations, 1 error
    turn      tool_call        {"tool":"read"}
    turn      tool_error       {"tool":"cargo_test"}
    turn      turn.iteration   {"steps":3}
    …
```

This is what makes the loop *evidence-based* — it can branch on its own runtime observations. The
trail is also durable: at the end of every turn the engine flushes new entries to the session's event
store, so a session's evidence survives process exit and can be read offline.

## Read & customize the loop — `flux loop`

```bash
flux loop show            # print the active loop (built-in or override) and its source
flux loop eject           # write the built-in to .flux/agent-loop.flux so you can edit it
flux loop eject --force   # overwrite an existing override with the built-in
```

A workspace `.flux/agent-loop.flux` **overrides** the built-in loop — the engine parses and runs it
on the next turn (an invalid override fails the turn rather than silently falling back). `eject` is a
convenience that drops the built-in text there for you to edit; you can also write the file by hand.
Because the loop is ordinary Flux-Lang, you can raise the iteration cap, add a stop condition, emit
extra observations, or restructure the control flow entirely — all within the same envelope.

See the [CLI](./cli.md) page for related commands, and the
[flux source](https://github.com/codewandler/flux) for the built-in loop and op reference.

## Related docs

- [Concepts](../concepts.md) — plans, symbols, evidence, and runtime ownership.
- [Safety and approvals](./safety.md) — the envelope every loop-dispatched operation crosses.
- [CLI](./cli.md) — commands for inspecting and ejecting the loop.
