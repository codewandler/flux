---
id: C-710
title: "A story worker validates with the gate's own commands, not a narrower guess"
pillar: "Core"
status: backlog
epic: fleet-harness-throughput
areas: [flux-cli, flux-orchestrate]
note: "wave-602 went red on clippy::field-reassign-with-default in test-only code; the worker ran 'clippy -p flux-cli -- -D warnings' and the gate ran '--workspace --all-targets'"
---

# A story worker validates with the gate's own commands, not a narrower guess

## Goal

A story worker reports its work green, hands off, and the repository gate then fails on something
the worker could not have seen — because the worker invented its own validation argv and the gate
runs a different one. The two commands are never reconciled, so the difference is discovered at the
most expensive possible moment: after integration has assembled the candidate and spent a full gate
run.

Measured on `wave-602`. The worker's own handoff reports:

    cargo clippy -p flux-cli -- -D warnings   → clean

The repository gate runs:

    cargo clippy --workspace --all-targets -- -D warnings

The lint that failed — `clippy::field-reassign-with-default`, on a two-line test fixture the worker
itself added — lives in a **test target**. `--all-targets` sees it; the worker's command cannot. The
worker was not careless: it ran clippy, it read clean, and it was telling the truth about the command
it ran. Cost: one full gate cycle (~10 min of `npm ci` + `release-full-gate.sh` + workspace clippy),
a red wave, a hand-authored fix commit and a re-handoff, for a defect worth two lines.

This is the same defect class as [C-664](C-664-cargo-test-workspace-lib-never-tests-flux-cli.md) seen
from the other end. C-664 is about the *install* gate silently skipping a crate; this is about the
*worker* validating a narrower scope than the gate that judges it. Fixing C-664 does not fix this,
because the worker would still be guessing.

The worker cannot solve this by trying harder. Nothing in its assignment tells it what the gate will
run — `final_gate` is fleet topology, host-side, and never reaches the model. Asking the worker to
guess a superset is how you get workers running the entire repository gate in every worktree, which
is exactly what `fleet.toml` deliberately refuses ("Full repository gates are the integrator's job
and run host-side, never from a worker's model").

The fix is to stop making it a guess: derive the worker's targeted validation from the same
configuration the gate is derived from, narrowed to the worker's write set rather than reinvented.

## Acceptance

- [ ] A worker's targeted validation argv is **derived** from the repository's configured gate, not authored independently. Narrowing by package or path is allowed; changing the *checks* is not — if the gate runs clippy with `--all-targets`, so does the worker's clippy.
- [ ] The derivation is visible to the worker as part of its assignment, so its handoff can quote the exact argv it was given rather than one it composed.
- [ ] A worker's green targeted validation and a red repository gate on the *same check* is a reportable inconsistency, not a silent surprise: the wave's red status names which check diverged and what the worker ran instead.
- [ ] The worker still does not run full repository gates in its worktree. This narrows scope; it must not widen authority — the `fleet.toml` boundary that keeps host-side gates out of a worker's model is unchanged.
- [ ] Failing first: a test drives a worker whose narrowed command passes while the gate's equivalent fails (the wave-602 shape — a lint in a test target invisible without `--all-targets`), and asserts the derived argv catches it before handoff.

## Notes

Found by reading `wave-602`'s worker transcripts after the gate went red. The worker's own report is
the clearest statement of the problem, because it lists the exact commands it ran and every one of
them was a reasonable choice.
