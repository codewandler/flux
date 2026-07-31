---
id: C-376
title: Let flow_run address a workspace flow path and return a route receipt
pillar: Agent
status: backlog
epic: harness-route-integrity
design: docs/designs/harness-route-integrity.md
note: "flow_run takes {name, inputs?} with deny_unknown_fields and resolves only inside .flux/flows|.flux/ops; examples/commit.flux is unreachable from the op by construction while the CLI resolves a file path first"
---

# Let `flow_run` address a workspace flow path and return a route receipt

## Goal

Give the agent the same addressing the CLI has, so a request naming a literal workspace `.flux`
file has a model-facing route at all — and make the result say which flow ran.

## Acceptance

- [ ] `FlowRunInput` (`crates/flux-tools/src/flows.rs:343-352`) accepts a workspace-relative path,
      resolved through `System` and workspace-confined, mutually exclusive with `name` — mirroring
      `load_cli_flow_target`'s file-first order (`crates/flux-cli/src/flow_cmd.rs:209-226`).
- [ ] The `flow_run` result carries the resolved path, the flow name and the seeded input keys;
      today it returns `{result, transcript, steps, suspension}` with no flow identity
      (`crates/flux-flow/src/loop_host.rs:711-719`). C-379's completion check has nothing to match
      against without this.
- [ ] Failing-first: a flow written outside `FLOW_DIRS` resolves and runs; a path escaping the
      workspace is refused.
- [ ] `docs/stories/L-79-run-saved-flows-cli.md`'s recorded decision that agent-side `flow_run` stays
      "compatibility-lenient" is revisited explicitly, not silently reversed.

## Progress

- 2026-08-01 — filed from validation of HAR-01. The path/name asymmetry is a recorded consequence of
  L-79, which is why this is a deliberate revision rather than a bug fix.

## Notes

- `examples/commit.flux:4` and `examples/README.md:101` both document
  `flux flow run examples/commit.flux` — a CLI-only invocation today.
