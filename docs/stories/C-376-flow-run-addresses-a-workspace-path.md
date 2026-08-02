---
id: C-376
title: Let flow_run address a workspace flow path and return a route receipt
pillar: Agent
status: done
epic: harness-route-integrity
design: docs/designs/harness-route-integrity.md
note: "flow_run accepts exactly one stored name or confined workspace-relative path, rereads path source per call, lowers against the live catalog, and returns a route receipt"
---

# Let `flow_run` address a workspace flow path and return a route receipt

## Goal

Give the agent the same addressing the CLI has, so a request naming a literal workspace `.flux`
file has a model-facing route at all — and make the result say which flow ran.

## Acceptance

- [x] `FlowRunInput` (`crates/flux-tools/src/flows.rs:343-352`) accepts a workspace-relative path,
      resolved through `System` and workspace-confined, mutually exclusive with `name` — mirroring
      `load_cli_flow_target`'s file-first order (`crates/flux-cli/src/flow_cmd.rs:209-226`).
- [x] The `flow_run` result carries the resolved path, the flow name and the seeded input keys;
      today it returns `{result, transcript, steps, suspension}` with no flow identity
      (`crates/flux-flow/src/loop_host.rs:711-719`). C-379's completion check has nothing to match
      against without this.
- [x] Failing-first: a flow written outside `FLOW_DIRS` resolves and runs; a path escaping the
      workspace is refused.
- [x] `docs/stories/L-79-run-saved-flows-cli.md`'s recorded decision that agent-side `flow_run` stays
      "compatibility-lenient" is revisited explicitly, not silently reversed.

## Progress

- 2026-08-01 — filed from validation of HAR-01. The path/name asymmetry is a recorded consequence of
  L-79, which is why this is a deliberate revision rather than a bug fix.
- 2026-08-02 — Activated by the project-adaptive review flow: the harness can see `flow_run`, but it
  cannot address `examples/review.flux`. Adding failing-first coverage for workspace confinement,
  per-call source freshness, mutual exclusion, and the route receipt before changing the operation.
- 2026-08-02 — DONE. The three new tests first failed because `path` was rejected and `name` was
  mandatory, then passed after the guarded path route landed. `flow_run` now declares the source
  read honestly, rereads workspace paths for each call, re-enters the authored-flow host against its
  live catalog, and returns `{route: {operation, resolved_path, flow_name, seeded_input_keys}}`.
  Updated both operation references, agent/tooling docs, design decisions, customer and engineering
  changelogs, and L-79's compatibility decision. Green: full workspace build/test/Clippy with
  warnings denied, workspace formatting, `flux-codegate`, both operation-reference coherence tests,
  and customer-changelog mirror. The full suite also caught and fixed L-129's example census pin.

## Notes

- `examples/commit.flux` and `examples/review.flux` remain outside the saved-flow homes and therefore
  do not appear in `flow_list`; the model-facing address is the explicit
  `flow_run({path: "examples/<name>.flux"})` form.
