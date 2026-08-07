---
id: C-664
title: "cargo test --workspace --lib never tests flux-cli"
pillar: "Core"
status: ready
priority: 4
areas: [flux-codegate]
epic: fleet-harness-throughput
---

# cargo test --workspace --lib never tests flux-cli

## Goal

`cargo test --workspace --lib` runs no `flux-cli` tests at all. `flux-cli` is a binary crate with no
library target, so `--lib` silently matches nothing there — and `flux-cli` holds
`board_fleet_cmd.rs`, which is the board and fleet implementation plus 392 of its tests.

This is a gate hole, not a preference. `task install` runs exactly that command, so the install gate
has never covered the crate most changed by fleet work. The tests only run when someone remembers
`--bins`.

## Acceptance

- [ ] The install/verification path runs `flux-cli`'s tests; `--lib` alone must not be what decides
      whether they execute.
- [ ] A test asserts that every workspace crate carrying tests is reached by the declared gate
      command, so a future binary-only crate cannot fall out of it silently.
- [ ] The story records which other crates, if any, were also uncovered.
- [ ] `task install` fails when a `flux-cli` test fails.
