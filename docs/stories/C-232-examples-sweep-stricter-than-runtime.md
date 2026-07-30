---
id: C-232
title: "The examples validation sweep is stricter than the runtime, so a valid Program cannot ship as an example"
pillar: Core
status: done
priority: 27
design:
note: "SURFACED BY A-117: examples_validate asserts flow_named(&t.run).is_some() for every trigger, but an agent-bound trigger legitimately parses with run == \"\" and flux-app's Engine::validate explicitly exempts it — the sweep does not, so an agent-triggered Program is unshippable as an example"
---

# The examples validation sweep is stricter than the runtime, so a valid Program cannot ship as an example

## Goal
`validate_program_structure` (`crates/flux-eval/tests/examples_validate.rs:88-99`) asserts
`flow_named(&t.run).is_some()` for **every** trigger. But an **agent-bound trigger** legitimately
parses with `run == ""` (`crates/flux-lang/src/cst_decode.rs:1555`), and the runtime knows this:
`flux-app`'s `Engine::validate` **explicitly exempts** that shape
(`crates/flux-app/src/app.rs:1003-1017`).

So the example sweep rejects a Program the runtime accepts. The practical consequence is not
cosmetic: a Program whose trigger drives an `agent` rather than a named `flow` — the shape the fleet
coordinator needs — **cannot ship as an example at all**, because the test that guards
`examples/` fails on valid input.

## Acceptance
- [x] `validate_program_structure` accepts an agent-bound trigger (`run == ""`) exactly as
      `Engine::validate` does. **Failing-first test**: an example Program with an agent-bound trigger
      passes the sweep — it fails today.
- [x] The sweep and the runtime agree **by construction, not by coincidence**. Two hand-maintained
      copies of "what is a valid trigger" is how this drifted in the first place, so either the test
      calls the runtime's own validation, or a comment at both sites names the other and says they
      must move together. Prefer the former.
- [x] The sweep does not get *looser* than the runtime in the process: a trigger naming a flow that
      genuinely does not exist must still fail. Pin both directions.
- [x] Standard gate green in both workspaces.

## Progress
- 2026-07-29 — surfaced by A-117 while it was blocked on other gaps. It is independent of the fleet
  work and deliberately filed on its own.

## Notes
- Seams: `crates/flux-eval/tests/examples_validate.rs:88-99`, `crates/flux-app/src/app.rs:1003-1017`,
  `crates/flux-lang/src/cst_decode.rs:1555`.
- **Why it stayed hidden:** every existing example uses a flow-bound trigger, so the stricter branch
  was never exercised. The first Program to need an agent-bound trigger is the one that trips it,
  which makes this look like a bug in the *new* Program rather than in the sweep.
- Related but separate, and worth fixing while in the area: the fleet-coordinator design doc's §2
  state diagram is stale against the authoritative `EDGES` table
  (`crates/flux-datasource/src/board.rs:74-85`, flagged in-code at `:71-73`). Doc-only, no behaviour.
