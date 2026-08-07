---
id: C-682
title: "Handoff evidence anchors are declared per repository, not derived by cargo-specific code"
pillar: "Core"
status: backlog
epic: fleet-harness-throughput
areas: [flux-cli]
note: "decision 0017 point 6; handoff_evidence.py is 170 lines of cargo-specific derivation and silently skips a story that adds tests to an existing file"
---

# Handoff evidence anchors are declared per repository, not derived by cargo-specific code

## Goal

A repository declares how its evidence is anchored, and the binary derives a handoff from that
declaration — instead of `handoff_evidence.py` inferring it with cargo-specific code.

The current deriver resolves crate names, finds added test functions and builds a `cargo test -p
<crate> <testname>` argv. That is 170 lines of one build system's conventions, and it is the reason
handoff derivation cannot move out of the roadmap repository: a Node or Python member has no crate to
resolve. Decision 0017 names this as one of the two missing stories because Design A cannot retire
the python without it.

It also has a live correctness cost, not only a portability one. The deriver anchors on test **files
a commit adds**. `flux/C-621` added four `#[test]` functions to an existing file, so it derived
nothing, was silently skipped, and left `wave-472` unable to integrate on
`conflict/precondition: flux/C-621 has no accepted handoff` — a wave held open by a story whose work
was in fact complete. An anchor that only recognises new files misreads "tests added to an existing
file" as "no tests".

The `gate` key in `[[repositories]]` is the pattern: the repository states its own command, and the
binary runs it without knowing what a workspace is.

## Acceptance

- [ ] A repository declares its evidence anchor in `fleet.toml` — how to name a targeted validation for a set of changed paths — and nothing in the binary assumes cargo, crates or Rust.
- [ ] Derivation anchors on added **test functions** as well as added test files, so a commit that extends an existing test file yields a valid argv. `flux/C-621` is the regression case.
- [ ] When no anchor can be derived, the refusal names why, and the story is left for a human rather than handed off on a guess — the current silent skip is what stranded wave-472.
- [ ] Host verification is unchanged: the argv is re-run at the pinned base, where it must fail or match zero tests, and at the commit, where it must pass. A wrong derivation stays refused, not accepted.
- [ ] Proven on a non-cargo member, or on a fixture repository whose anchor is not a cargo command.
- [ ] `handoff_evidence.py` is deleted when this lands, per decision 0017's Consequences.
