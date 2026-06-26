# Self-improvement

flux improves its own harness, and proves the improvement. It runs itself against real
coding/shell benchmarks, mines the failures, derives + implements a candidate fix, re-evaluates,
and **keeps the change only if it measurably helps** (with the dev-gate green) — otherwise it
reverts. The whole loop is authored as a flux-flow graph and executed by `flux flow run`; the agent
never grades itself, declares victory, or hides the evidence.

This folder is the operator- and reviewer-facing record of that work.

- **[DESIGN.md](DESIGN.md)** — how the process works: the loop, the three invariants and how they're
  enforced, scoring, the sub-agent roles, observation tooling, and how to run it.
- **[STATUS.md](STATUS.md)** — where the journey stands: what's proven end-to-end, the bugs each live
  run surfaced and how they were fixed, the first kept gain (with its honest caveats), the known gaps,
  and what's next.

Related docs & code:

- [`crates/flux-eval/`](../../crates/flux-eval) — the L3 crate that implements the loop ops and
  benchmark adapters (see [DESIGN.md → Where the code lives](DESIGN.md#where-the-code-lives)).
- [`docs/designs/flux-flow.md`](../designs/flux-flow.md) — the pure-DAG engine the loop is authored in.

The loop flow itself lives at [`examples/improve-tbench.flux`](../../examples/improve-tbench.flux)
(terminal-bench) and [`examples/improve.flux`](../../examples/improve.flux) (local suite). Run it with
[`bench/run-tbench-loop.sh`](../../bench/run-tbench-loop.sh).
