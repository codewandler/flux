---
id: A-149
title: "Route greenfield Node work to dedicated tooling from task intent"
pillar: Agent
status: done
epic: harness-route-integrity
design: docs/designs/harness-route-integrity.md
areas: [flux-tools, flux-flow]
note: "Split from C-381 after a live flux-bench race: explicit Vue/npm work in an empty workspace routed to the virtual process family, which contained no executable tool"
---

# Route greenfield Node work to dedicated tooling from task intent

## Goal

Let an explicit Node ecosystem request surface the dedicated `npm` and `node_run` operations even
when a greenfield workspace has no package marker yet. Keep the generic `bash` and `proc.run` escape
hatches operator-gated.

## Acceptance

- [x] The built-in `node` group carries bounded `turn.intent` hints for explicit Node ecosystem
      terms used before a project exists, including npm, package.json, and Vue.
- [x] The existing staged family-index path can discover `node` from those hints and advertise its
      dedicated operations without requiring an ambient project signal.
- [x] The `shell` group gains no intent hints and remains available only through its existing
      operator-controlled signal.
- [x] A failing-first regression pins the Node hints and the unchanged shell boundary.
- [x] The user-visible routing correction is recorded in `CHANGELOG.md` and `WHATS-NEW.md`.
- [x] The standard validation gate passes.

## Progress

- 2026-08-05: Filed from a live flux-bench and direct tmux race against flux 0.55.0. A greenfield
  Vue/Vuex todo task requested npm-backed tests, but routing selected `process`; that virtual family
  contained Git operations and exposed neither `npm` nor `node_run`. A dependency-free JavaScript
  control task showed the same missing-runtime pattern.
- 2026-08-05: Added a failing-first manifest regression; it failed because `npm` had no
  `turn.intent` matcher, then passed after adding the bounded Node ecosystem hints. `cargo fmt --all
  --check` passed. `cargo test --workspace` exhausted the filesystem while linking unrelated
  workspace targets (`No space left on device`), so the workspace suite did not complete and Clippy
  was not reached. Removed the task-owned 11 GB worktree `target/` afterward; the full gate remains
  owed.
- 2026-08-05: Retried `cargo test --workspace` with `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2`.
  Compilation reached the final SDK examples and tests, then `rust-lld` terminated with `SIGBUS`
  while the filesystem reported zero available bytes. Removed the retry's task-owned 13 GB
  `target/`; no workspace test assertion ran red, but the full test gate and Clippy remain owed.
- 2026-08-06: Recovered the implementation onto the current canonical base, resolved its release
  note conflicts without taking superseded release text, and added a production-registry staged
  routing regression. A greenfield Vue/npm request now proves that the `node` family index contains
  and advertises `npm` plus `node_run`, while `shell`, `bash`, and `proc.run` remain inactive. The
  regenerated customer changelog and embedded public docs are in sync. `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check`, and
  `cargo test -p flux-codegate` all pass.

## Notes

- C-381 retains the broader measurement cassette, intent-schema, and first-party-family program.
  This story owns only the concrete Node greenfield regression so the live defect can land without
  claiming the wider story complete.
- Adding `shell` as an automatic fallback would violate the evidence-gated surfacing boundary. The
  fix must route to the least-capable dedicated tool group instead.
