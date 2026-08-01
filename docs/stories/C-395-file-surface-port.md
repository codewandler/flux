---
id: C-395
title: "State the workspace-confined file surface as a port"
pillar: Core
status: ready
priority: 7
design: docs/designs/execution-substrate.md
epic: execution-substrate
note: "C-269 deferred this on the stated grounds that its consumers all hold a concrete System, so a trait would be indirection without a seam — a second consumer is exactly the condition that expires that reasoning"
---

# State the workspace-confined file surface as a port

## Goal

`port.rs` states `GuardedEnv`, `GuardedProcess` and `GuardedHostFiles`, and deliberately omits the
workspace-confined file surface (`read_file`, `write_file`, …). Add it, so a consumer that holds only
the port gets the same confinement a consumer holding a concrete `System` gets.

## Acceptance

- [ ] The workspace-confined file operations are reachable through a trait in `flux_system::port`,
      with the native `System` as the first implementor by pure delegation.
- [ ] **Failing-first test** — a consumer holding only the trait is refused the same escapes a
      concrete-`System` consumer is refused: a lexical `..`, and a symlink that canonicalizes outside
      the root. The test must fail before the delegation is written, and it must exercise the
      *trait*, not the struct.
- [ ] Read/write asymmetry survives the port: `read_roots` remain readable and not writable through
      the trait, exactly as through the struct.
- [ ] Optional operations default to denial, matching the existing port's fail-closed posture.
- [ ] `cargo test -p flux-codegate` stays green with no new backend allowance.

## Progress
- (not started)

## Notes
- The deferral this closes is recorded in `crates/flux-system/src/port.rs` — *"a trait with no call
  sites would be indirection without a seam"*. That was correct; the seam now exists.
- Do **not** add a god trait. The port is split by guarded resource on purpose; a consumer names only
  what it uses.
