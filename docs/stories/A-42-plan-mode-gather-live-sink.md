---
id: A-42
title: Stream plan-mode gather rounds to the live sink
pillar: Agent
status: backlog
epic: multipass-agent-loop
note: "A-18 deferred this: plan-mode gather dispatch runs through a NullSink because reborrowing `&mut dyn AgentSink` per round hits an NLL wall — the fix is the loop host's ChannelSink/drain-loop shape; UX only, the envelope is unaffected"
---

# Stream plan-mode gather rounds to the live sink

## Goal
A-18's plan-mode gather executes read-only plans through an internal `NullSink`
(`FlowEngine::compile_with_gather`, crates/flux-flow/src/engine.rs), so the user sees silence
while gather rounds run instead of the live op/spinner stream normal mode shows. The borrow
structure (one `&mut dyn AgentSink` reborrowed per round inside the loop) hits a hard NLL wall
(E0499/E0505/E0597); the loop host solved the same problem with a `ChannelSink` + drain loop.
Give plan mode the same shape so gather is visible.

## Acceptance
- [ ] Plan-mode gather rounds stream ops/results to the caller's sink live (CLI `flux plan` and
      REPL `/plan` both show them), mirroring normal-mode rendering (A-15 labels).
- [ ] No change to what executes (read-only gather, shared budget) — rendering only.
- [ ] Failing-first test on the seam (a recording sink observes gather-round events).

## Progress
- 2026-07-06 filed — the scope-limiting deviation recorded in A-18's Progress.

## Notes
- See A-18's Progress entry for the exact NLL shape attempted (loop + Box::pin recursion both
  failed); the ChannelSink/drain-loop pattern in `loop_host.rs` is the known-good architecture.
