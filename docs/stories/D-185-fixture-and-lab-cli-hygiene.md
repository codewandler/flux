---
id: D-185
title: Fixture and Lab-CLI hygiene — record/test name validation, atomic session copy, diff rendering
pillar: Agent
status: ready
epic: deterministic-agent-lab
design: docs/designs/deterministic-agent-lab.md
priority: 4
note: "review finding (2026-07-28): minor hygiene batch across flux record/test, copy_session_to, and the run-diff fallback renderer"
---

# Fixture and Lab-CLI hygiene — record/test name validation, atomic session copy, diff rendering

## Goal
Batch of confirmed minor defects from the 2026-07-28 epic review, none release-blocking, all worth
closing before the Lab sees external use:

1. **`flux record` accepts `.` and `..` as scenario names** (`crates/flux-cli/src/lab_cmd.rs` —
   the check rejects separators but not dot segments): `flux record .. "x"` targets
   `tests/scenarios/..` = `tests/`, and because no `scenario.toml` exists there the clobber guard
   never trips — fixture files land outside the scenarios root.
2. **`flux test <name>` has no name validation at all** (`discover_fixtures` does `dir.join(name)`),
   so `flux test ../../anywhere/fixture` replays an arbitrary path outside `--dir`. Asymmetric with
   `run_record`'s guard.
3. **`EventStore::copy_session_to` is not atomic** (one transaction per event): a mid-loop failure
   leaves a partially-copied destination session already minted in the registry; a retry creates a
   second half-session beside the orphan. Fresh ULIDs mean no corruption — the defect is stranded
   half-sessions listing as real ones.
4. **Fixture timestamp inversion**: the destination's `SessionStarted` is stamped `now_ms` while
   copied events keep their original older `ts_ms`, so fixture sessions show
   `created_at > updated_at`. Cosmetic.
5. **Fallback diff rows render as raw keys**: `cell_rows` synthesizes `stmt = "call:{op}:{hash16}"`,
   which never resolves through `stmt_texts`, so `render_run_diff` prints `<call:op:abcd…>` instead
   of something readable for natively-dispatched runs.

## Acceptance
- [ ] `flux record` and `flux test` both reject any name that is not a single plain path segment
      (no separators, not `.`/`..`); tests pin both commands.
- [ ] `copy_session_to` copies atomically (single transaction per backend, or
      write-then-register-last so a partial copy is never listed); test pins that a failed copy
      leaves no listed session.
- [ ] Fixture sessions carry a `SessionStarted`/registry timestamp consistent with the copied
      events (no `created_at > updated_at`).
- [ ] `render_run_diff` prints a readable label for synthesized cell rows (e.g. the op name and
      hash prefix) instead of an unresolvable `<call:…>` placeholder.
- [ ] `--store` with a relative path: either resolve to an absolute path before exporting
      `FLUX_STORE_DIR`, or document that subprocesses with a different cwd resolve it elsewhere.

## Progress
- (not started)

## Notes
- The `.gitignore` sidecar gap from the same review (`*.db-wal`/`*.db-shm` not un-ignored under
  `tests/scenarios/`) was fixed directly on 2026-07-28 alongside the review; this story is the
  remainder.
