---
title: Time Machine
description: "Replay, fork, and diff recorded runs with resumable plans and deterministic re-execution."
---

# Time Machine — replay, fork, diff

Time Machine is the run-history toolkit: replay an old run without live IO, fork from a recorded
decision point, or diff two executions. It works because flux records accepted plans and leaf-operation
results as durable artifacts.

Two recordings make a run travelable:

- Every accepted plan is persisted as canonical, re-parseable Flux-Lang text — so re-executing a
  past run **needs no model call**.
- Every leaf-op dispatch (a file read, a shell command, an HTTP call) records a redacted
  **cassette** cell — op, input hashes, output — on the session's event stream, so op outputs are
  durable and re-execution **needs no live IO** either.

Three verbs operate on any recorded run:

| Command | What it does |
|---------|--------------|
| `flux replay <session>` | re-execute a past run exactly — offline, model-free, side effects never re-fire |
| `flux fork <session> --at N` | branch a run at a decision point and continue differently |
| `flux diff <A> <B>` | align two runs and show where the plan, or the world, diverged |

Use `flux sessions` to list recent session ids (`s_42`-style); every verb also accepts `last`.

## `flux replay` — hermetic re-execution

```bash
flux run "summarize Cargo.toml"   # a normal run records plans + cassette automatically
flux replay last                  # re-execute it: no API call, no live IO
flux replay s_42 --turn 2         # replay only the second turn's plans (1-based)
flux replay s_42 --sub-agents     # also replay spawned sub-agent child streams, in spawn order
flux replay s_42 --json           # machine-readable report
```

Plans re-parse from the stored plan text; op outputs are served from the cassette. No model client
is ever constructed, no side effect re-fires, and `confirm` gates auto-allow — nothing can actually
execute from tape. The transcript renders like the original, minus the latency, ending with a
summary:

```
replayed 3 plan(s) · 7/7 recorded cell(s) served
```

Replay reproduces the recorded loop's dispositions (skipped identical plans, halted prefixes,
revision fast-forwards) and tolerates the nondeterministic interleaving of `parallel` branches. Any
mismatch between the re-executed run and the recording is a loud divergence error with **exit code
1** — never a silent continuation. With `--turn`, a statement that references a symbol bound in an
earlier turn fails honestly rather than fabricating a value.

## `flux fork` — branch at a decision point

```bash
# Branch s_42 at top-level statement 2 (0-based, of its final executed plan):

# Mode A: inject a different value as that statement's result, run the rest live
flux fork s_42 --at 2 --inject '{"status": "degraded"}'

# Mode B (default): let the model re-plan the tail from the forked state
flux fork s_42 --at 2 --replan --prompt "try the staging environment instead"

# Mode C: continue with a corrected plan file (.flux text or DraftAst JSON);
# unchanged leading statements fast-forward, edits run live
flux fork s_42 --at 2 --edit fixed-plan.flux
```

The fork creates a **new session**, correlated to its source. The prefix before `--at` replays
hermetically from the cassette — no side effects — then execution crosses the cassette-vs-live
boundary: the tail runs **live**, through the real approval envelope, against today's world. The
three modes are mutually exclusive; `--prompt` refines `--replan` (default: continue the recorded
task). Agent flags (e.g. `-m` for the re-planning model) apply as on `flux run`.

The forked session records its own cassette and plan text, so a fork is itself replayable — and
diffable against its parent:

```bash
flux fork last --at 1 --replan
flux diff s_42 s_43
```

## `flux diff` — where did two runs diverge?

```bash
flux diff s_42 s_43        # e.g. a run and its fork
flux diff s_42 last --json
```

`flux diff` aligns the two runs' executed top-level statements positionally and classifies each
row:

- `=` — identical statement, identical recorded outputs.
- `~ plan diverges` — the statement itself differs (the model, or an edit, chose differently).
- `≠ same statement, different world` — the same plan step got a different recorded op output
  (a file changed, a command returned something else).

Exit code is `1` when the runs diverge, `diff`-style, so it composes in scripts. Positional
alignment is designed for run-vs-fork pairs; unrelated runs will mostly report plan divergence.

## Cassettes

Capture is **on by default** for every run, including `flux flow run`. Each recorded cell costs
roughly 442 bytes on representative ops — about 0.01% of a heavily-used event log — so the default
is cheap.

- **Redaction** — cell contents pass through the same redactor that scrubs stored plan text before
  anything hits disk, so recorded op outputs carry the same secret-scrubbing posture as the rest of
  the durable event log.
- **Per-op cap** — `FLUX_CASSETTE_MAX_BYTES` (default 1 MiB). An over-cap output keeps a truncated
  head and is marked as such; replay refuses truncated cells loudly rather than serving a partial
  world.
- **Kill switch** — `FLUX_CASSETTE=0` disables capture entirely. Runs recorded this way (and runs
  from before the cassette existed) cannot be replayed or forked; the commands say so explicitly.

## Resumable stored flows

Authored flows get a related capability on the [`flux flow run`](./cli.md) path — halt, fix, and
resume without repeating completed work:

```bash
flux flow run deploy.flux --resumable
# … a failed statement (or a paused `await`) prints a structured halt report:
# ✓/✗/· marked statement tree, a machine-readable failure summary, and the session id,
# then exits non-zero instead of erroring the whole run.

# Fix the flow file, then resume that session:
flux flow run deploy.flux --resume s_57
flux flow run deploy.flux --resume last
```

`--resume` (which implies `--resumable`) re-parses the — possibly corrected — file, folds the
halted session's statement ledger, fast-forwards the completed prefix (bound values are
rehydrated, side effects are not repeated), and executes from the first changed statement.
`--resume last` needs the flow to declare a name (`flow <name> -> …`) so its most recent halted
session can be found unambiguously; unnamed flows need the explicit session id. For the in-language
durability markers that pair with this (`await`, `checkpoint`, `once`), see
[Durability](../language/durability.md).

## Limits

- **Only recorded runs travel.** Sessions captured with `FLUX_CASSETTE=0`, or predating cassette
  capture, are not replayable or forkable.
- **The model is not replayed.** The cassette records leaf-op dispatches; the outer agent loop
  (the model calls themselves) is never cassetted. Replay re-parses the *accepted* plans — it
  reproduces what the run did, not the token stream that produced it.
- **A fork's tail is live.** Past the divergence point, ops act on today's world. That is the
  point — and it is gated by the same approval envelope as any live run — but it means fork output
  depends on current state, and `--replan` is nondeterministic by design (it is a real model turn).
- **Divergence is fatal, by design.** If the world leaked into a run in a way the recording cannot
  explain, replay stops with an error instead of improvising.

## Related docs

- [CLI](./cli.md) — the full command surface, including `flux sessions` and `flux flow run`.
- [Durability](../language/durability.md) — `await`, `checkpoint`, `once`, `saga`.
- [Execution model](../language/execution-model.md) — why flux runs are deterministic.
- [Concepts](../concepts.md) — plans, symbols, and the safety envelope.
