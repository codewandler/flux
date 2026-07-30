---
id: C-213
title: "Extract the harness discovery + scan layer out of the CLI binary into flux-capabilities"
pillar: Core
status: done
priority: 10
epic: harness-history
design: docs/designs/harness-history.md
note: "the knowledge of where ~/.codex, ~/.claude/projects and opencode.db live is real, tested and trapped in a 2919-line module inside a binary crate; nothing else in the tree can call it"
---

# Extract the harness discovery + scan layer out of the CLI binary into `flux-capabilities`

## Goal
`flux usage` knows how to find and read the local state of four harnesses, and that knowledge is
correct, exercised by 12 tests, and **unreachable**: it lives in `crates/flux-cli/src/usage.rs`, a
2919-line module inside a binary crate. Move the reusable half — discovery, file/row iteration, and
the scan budget — into `flux-capabilities` so C-214 can build a message-shaped model on it, leaving
`flux usage` behaviourally identical on top of the same code.

This is a pure refactor. It ships no new user-visible behaviour, and that is the point: it is the
story that makes the next three cheap, and it is the only one in the epic that can be verified purely
by "nothing changed".

## Acceptance
- [x] A `harness` module in `flux-capabilities` owns: `HarnessKind` (`flux | codex | claude-code |
      opencode`), root discovery including the env overrides (`CODEX_HOME`, `CLAUDE_CONFIG_DIR`,
      `OPENCODE_DATA_DIR`, and the `~` fallbacks), the not-found / not-readable outcomes as typed
      values rather than as rendered strings, and the scan budget (`MAX_JSONL_FILES`,
      `MAX_JSONL_FILE_BYTES`) with its skip-and-count degradation.
- [x] `flux usage` consumes it and keeps its own token-shaped projection. **Failing-first is not
      available for a pure refactor, so the pin is the inverse**: the 12 existing `usage.rs` tests
      pass unchanged, with no edits to their assertions. An edited assertion in this story is a
      behaviour change in disguise and must be justified in the story's Progress or reverted.
- [x] A characterization test locks the discovery precedence per harness — env override wins, then
      the `~` default, then "missing" — since that precedence is currently implicit in the
      `or_else` chains and is the thing most likely to be silently altered by the move.
- [x] Every adapter still opens read-only; the opencode path keeps `SQLITE_OPEN_READ_ONLY`.
- [x] Layering holds: `flux-capabilities` is L5 and may depend on `flux-events` (L2). `cargo test -p
      flux-codegate` stays green.
- [x] Standard gate green in both workspaces.

## Progress
- 2026-07-29 — filed with the epic.
- 2026-07-30 — extracted. `flux_capabilities::harness` (`mod.rs` discovery + `scan.rs` iteration)
  now owns `HarnessKind`, `HarnessEnv`, `HarnessLocation`, `ScanBudget` and the SQLite read-only
  open; `usage.rs` lost 211 lines and kept every projection. The 12 `usage.rs` tests pass with **no
  assertion edited** — the only test-module change is a re-added `use std::fs::{self, File};`,
  needed because the parent module no longer imports them.
  - **The pin is not only "nothing changed".** A pure refactor's inverse pin is weak on its own, so
    the layer also ships four tests that *could not have been written before it*
    (`crates/flux-capabilities/tests/harness_discovery.rs`) — they run against the library with no
    `flux` binary, which is the property that makes C-214 cheap.
  - **Discovery takes an injected `HarnessEnv` rather than reading `std::env` per site.** That is
    what makes the precedence characterization test possible without mutating process-global state
    (racy under parallel tests, `unsafe` from Rust 2024). `run_usage` snapshots it once, so every
    harness in one run resolves against the same view.
  - **The budget became a value (`ScanBudget`), defaulting to the same two constants.** The epic
    calls it a correctness property; as bare constants it was untestable without a 200 MiB fixture,
    and C-214 is required to *tighten* it for message bodies. `flux usage` passes
    `ScanBudget::default()` everywhere, so its behaviour is byte-identical.
  - **`flux-capabilities` is a model-facing operation crate**, so the moved `fs`/`File`/`rusqlite`
    calls needed `// flux-allow-direct-io:` waivers that they did not need in `flux-cli`. Each
    states the same real reason: the path is environment-derived, never model-supplied, outside any
    workspace by construction, and opened read-only.
  - No manifest and no lockfile change: `flux-capabilities` already had `rusqlite` and `flux-core`,
    and `flux-cli` already depended on `flux-capabilities`. `flux-events` was **not** added — the
    flux adapter's discovery is a path, and only C-214 needs the `EventStore`.

## Notes
- Seams: discovery at `crates/flux-cli/src/usage.rs:816-880` (`collect_codex`, `collect_claude`,
  `collect_opencode`); budget constants at `:23-24`; the JSONL walker is `jsonl_files`.
- **Move the layer, not the projection.** `UsageRecord`, `SessionRecord`, `HarnessDataset`,
  cost/pricing and every render path stay in `flux-cli` — they are `flux usage`'s answer to its own
  question, not a shared model. What moves is only "where does this harness keep its state, and how
  do I walk it without falling over".
- Resist widening scope into the render/JSON layer of `usage.rs` while in there. If something needs
  fixing, file it (`/track:story`); a refactor whose diff also changes output cannot be verified by
  its own test suite, which is this story's only safety net.
- No new crate — see the epic's Notes and the standing repo preference for modules.
